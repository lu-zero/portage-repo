//! Metadata cache operations — regeneration and (future) bulk reading.
//!
//! [`regen_cache`] sources all ebuilds via [`crate::source::source_parallel`]
//! and writes the resulting `md5-cache` files to disk.
//!
//! The sourcing concern (running bash, extracting metadata) lives in
//! [`crate::source`]; this module owns the disk I/O side.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use camino::Utf8Path;
use portage_metadata::CacheEntry;

use crate::source::{SourceContext, SourceOpts, SourcedEbuild, source_parallel};
use crate::{Ebuild, Repository, Result};

type ChecksumCache = Arc<Mutex<HashMap<PathBuf, md5::Digest>>>;

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
    let ctx = SourceContext::new();
    let checksum_cache: ChecksumCache = Arc::new(Mutex::new(HashMap::new()));
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
    {
        let guard = cache.lock().unwrap();
        if let Some(&d) = guard.get(path.as_std_path()) {
            return Ok(d);
        }
    }
    let data = fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let digest = md5::compute(&data);
    cache
        .lock()
        .unwrap()
        .entry(path.to_path_buf().into_std_path_buf())
        .or_insert(digest);
    Ok(digest)
}

fn write_entry(
    ebuild: &Ebuild,
    sourced: SourcedEbuild,
    out_dir: &std::path::Path,
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

    let cat_dir = out_dir.join(ebuild.category());
    fs::create_dir_all(&cat_dir).map_err(|e| format!("mkdir: {e}"))?;
    fs::write(
        cat_dir.join(format!("{}-{}", ebuild.name(), ebuild.version())),
        entry.serialize(),
    )
    .map_err(|e| format!("write: {e}"))?;
    Ok(())
}
