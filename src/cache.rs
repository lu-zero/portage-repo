//! Metadata cache operations — regeneration and (future) bulk reading.
//!
//! [`regen_cache`] sources all ebuilds via [`crate::source::source_parallel`]
//! and writes the resulting `md5-cache` files to disk.
//!
//! The sourcing concern (running bash, extracting metadata) lives in
//! [`crate::source`]; this module owns the disk I/O side.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use camino::Utf8Path;
use portage_metadata::CacheEntry;

use crate::source::{SourceContext, SourceOpts, SourcedEbuild, source_parallel};
use crate::{Ebuild, Repository, Result};

/// Shared eclass file → md5 cache used across all regen workers.
///
/// `papaya::HashMap` gives lock-free reads; the first-miss race where two
/// workers concurrently read and hash the same eclass is benign because
/// `insert` is atomic and the digests are identical.
type ChecksumCache = Arc<papaya::HashMap<PathBuf, md5::Digest>>;

/// Options for [`regen_cache`].
#[derive(Debug, Clone, Default)]
pub struct RegenOpts {
    pub source: SourceOpts,
    /// Directory to write `md5-cache` files into. `None` = dry-run (source, don't write).
    pub output_dir: Option<PathBuf>,
}

/// Result counters returned by [`regen_cache`].
#[derive(Debug, Clone, Default)]
pub struct RegenStats {
    pub total: usize,
    pub errors: usize,
}

/// Source all `ebuilds` and optionally write `md5-cache` files.
///
/// `on_progress(completed, total)` is called after each ebuild finishes.
pub async fn regen_cache(
    repo: &Repository,
    masters: &[Repository],
    ebuilds: Vec<Ebuild>,
    opts: &RegenOpts,
    on_progress: impl Fn(usize, usize) + Send + Sync + 'static,
) -> Result<RegenStats> {
    let total = ebuilds.len();
    let out_dir = opts.output_dir.clone();

    // Pre-create one directory per category. Doing this upfront turns ~30k
    // per-ebuild create_dir_all calls into ~200 (one per category) and lets
    // the worker loop write without coordinating on directory state.
    if let Some(ref dir) = out_dir {
        let mut cats: HashSet<&str> = HashSet::new();
        for e in &ebuilds {
            cats.insert(e.category());
        }
        for cat in cats {
            let p = dir.join(cat);
            fs::create_dir_all(&p).map_err(|e| crate::Error::Io { path: p, source: e })?;
        }
    }

    let ctx = SourceContext::new();
    let checksum_cache: ChecksumCache = Arc::new(papaya::HashMap::new());
    let errors = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let on_progress = Arc::new(on_progress);

    source_parallel(
        repo,
        masters,
        ebuilds,
        &opts.source,
        &ctx,
        {
            let checksum_cache = Arc::clone(&checksum_cache);
            let errors = Arc::clone(&errors);
            let done = Arc::clone(&done);
            move |ebuild, result| {
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                on_progress(n, total);
                match result {
                    Err(e) => {
                        eprintln!("\nERROR {}: {e}", ebuild.cpv());
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(sourced) => {
                        if let Some(ref dir) = out_dir {
                            if let Err(e) = write_entry(&ebuild, sourced, dir, &checksum_cache) {
                                eprintln!("\nWRITE ERROR {}: {e}", ebuild.cpv());
                                errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
        },
    )
    .await?;

    Ok(RegenStats {
        total,
        errors: errors.load(Ordering::Relaxed),
    })
}

fn eclass_md5(
    path: &Utf8Path,
    cache: &ChecksumCache,
) -> std::result::Result<md5::Digest, String> {
    let pinned = cache.pin();
    if let Some(&d) = pinned.get(path.as_std_path()) {
        return Ok(d);
    }
    let data = fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let digest = md5::compute(&data);
    pinned.insert(path.to_path_buf().into_std_path_buf(), digest);
    Ok(digest)
}

fn write_entry(
    ebuild: &Ebuild,
    sourced: SourcedEbuild,
    out_dir: &Path,
    checksum_cache: &ChecksumCache,
) -> std::result::Result<(), String> {
    let ebuild_bytes = fs::read(ebuild.path()).map_err(|e| format!("read ebuild: {e}"))?;
    let ebuild_md5 = format!("{:x}", md5::compute(&ebuild_bytes));

    // Md5 every eclass that was actually sourced, using its resolved path.
    // This is path-accurate across master repos — a name-only lookup would
    // miss eclasses inherited from a master overlay's eclass/ directory.
    let SourcedEbuild { metadata, eclasses } = sourced;
    let eclasses: Vec<(String, String)> = eclasses
        .into_iter()
        .map(|(name, path)| {
            let digest =
                eclass_md5(&path, checksum_cache).map_err(|e| format!("eclass {name}: {e}"))?;
            Ok((name, format!("{digest:x}")))
        })
        .collect::<std::result::Result<_, String>>()?;

    let entry = CacheEntry { metadata, md5: Some(ebuild_md5), eclasses };

    // Write to `{name}.tmp` then rename — POSIX rename is atomic on the same
    // filesystem, so a crash mid-write can never leave a truncated cache file.
    // The category directory already exists (created up front in regen_cache).
    let cat_dir = out_dir.join(ebuild.category());
    let file_name = format!("{}-{}", ebuild.name(), ebuild.version());
    let final_path = cat_dir.join(&file_name);
    let tmp_path = cat_dir.join(format!("{file_name}.tmp"));
    fs::write(&tmp_path, entry.serialize()).map_err(|e| format!("write tmp: {e}"))?;
    fs::rename(&tmp_path, &final_path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}
