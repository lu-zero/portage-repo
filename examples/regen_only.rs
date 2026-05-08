//! Minimal metadata regeneration benchmark — no comparison, optional write.
//!
//! Sources every ebuild in the repo in parallel and optionally writes the
//! resulting md5-cache files to a directory.  Intended as a like-for-like
//! comparison with:
//!
//!   pk repo metadata regen -p <cache-dir> -n -f -j <N> <repo>
//!
//! Usage:
//!   regen_only <repo-path> [filter] [-o <cache-dir>] [-j <N>]
//!
//! Examples:
//!   regen_only gentoo
//!   regen_only gentoo 'dev-util/*'
//!   regen_only gentoo -o /tmp/portage-cache -j 12

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use portage_metadata::CacheEntry;
use portage_repo::{Ebuild, Repository};

type EclassChecksumCache = Arc<Mutex<HashMap<PathBuf, md5::Digest>>>;

fn eclass_md5(path: &Path, cache: &EclassChecksumCache) -> Result<md5::Digest, String> {
    {
        let guard = cache.lock().unwrap();
        if let Some(&digest) = guard.get(path) {
            return Ok(digest);
        }
    }
    let data = fs::read(path).map_err(|e| format!("read eclass {}: {e}", path.display()))?;
    let digest = md5::compute(&data);
    cache
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_insert(digest);
    Ok(digest)
}

/// Check whether an ebuild matches a glob-like filter (`cat/*` or `cat/pkg-ver`).
///
/// Uses `category()` to short-circuit before allocating a CPV string.
fn matches_filter(ebuild: &Ebuild, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    // Fast path: check category without allocating.
    let cat_end = filter.find('/').unwrap_or(filter.len());
    let filter_cat = &filter[..cat_end];
    if ebuild.category() != filter_cat {
        return false;
    }
    let rest = &filter[cat_end..]; // "/" or "/pkg*" or ""
    if rest == "/" || rest.ends_with("/*") && rest.len() == 2 {
        return true; // "cat/*"
    }
    // Fall back to full string for sub-package filters (rare).
    let cpv = ebuild.cpv().to_string();
    if let Some(prefix) = filter.strip_suffix('*') {
        cpv.starts_with(prefix)
    } else {
        cpv == filter
    }
}

async fn process_ebuild(
    repo: &Repository,
    masters: &[Repository],
    ebuild: &Ebuild,
    out_dir: Option<&PathBuf>,
    eclass_cache: &EclassChecksumCache,
) -> Result<(), String> {
    let master_refs: Vec<&Repository> = masters.iter().collect();
    let mut shell = repo
        .shell_with_masters(&master_refs)
        .await
        .map_err(|e| format!("shell: {e}"))?;

    let metadata = shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| format!("source: {e}"))?;

    if let Some(dir) = out_dir {
        // Compute MD5 of the ebuild file itself.
        let ebuild_bytes = fs::read(ebuild.path()).map_err(|e| format!("read ebuild: {e}"))?;
        let ebuild_md5 = format!("{:x}", md5::compute(&ebuild_bytes));

        // Compute MD5 checksums for each transitively inherited eclass.
        // Results are cached across workers so each eclass is hashed once.
        let eclasses = metadata
            .inherited
            .iter()
            .map(|name| {
                let path = shell
                    .eclass_path(name)
                    .ok_or_else(|| format!("eclass not found after sourcing: {name}"))?;
                eclass_md5(path.as_std_path(), eclass_cache)
                    .map(|d| (name.clone(), format!("{d:x}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let entry = CacheEntry {
            metadata,
            md5: Some(ebuild_md5),
            eclasses,
        };
        let category = ebuild.category();
        let cat_dir = dir.join(category);
        fs::create_dir_all(&cat_dir).map_err(|e| format!("mkdir: {e}"))?;
        let cpv_file = cat_dir.join(format!("{}-{}", ebuild.name(), ebuild.version()));
        fs::write(&cpv_file, entry.serialize()).map_err(|e| format!("write: {e}"))?;
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    #[cfg(feature = "dhat-heap")]
    let _dhat = dhat::Profiler::new_heap();

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "Usage: {} <repo-path> [filter] [-o <cache-dir>] [-j <N>]",
            args[0]
        );
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} gentoo", args[0]);
        eprintln!("  {} gentoo 'dev-util/*'", args[0]);
        eprintln!("  {} gentoo -o /tmp/portage-cache -j 12", args[0]);
        process::exit(2);
    }

    let repo_path = &args[1];
    let mut filter: Option<String> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut jobs: usize = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                if i < args.len() {
                    out_dir = Some(PathBuf::from(&args[i]));
                }
            }
            "-j" | "--jobs" => {
                i += 1;
                if i < args.len() {
                    jobs = args[i].parse().unwrap_or(jobs);
                }
            }
            _ => {
                if filter.is_none() {
                    filter = Some(args[i].clone());
                }
            }
        }
        i += 1;
    }

    let repo = match Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repo: {e}");
            process::exit(1);
        }
    };

    let mut ebuilds = match repo.ebuilds() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error listing ebuilds: {e}");
            process::exit(1);
        }
    };

    if let Some(ref f) = filter {
        ebuilds.retain(|eb| matches_filter(eb, f));
    }

    let total = ebuilds.len();
    let filter_desc = filter
        .as_ref()
        .map(|f| format!(" (filter: {f})"))
        .unwrap_or_default();
    eprintln!(
        "Sourcing {total} ebuilds with {jobs} workers{}{}...",
        filter_desc,
        out_dir
            .as_ref()
            .map(|p| format!(", writing to {}", p.display()))
            .unwrap_or_default()
    );

    let (tx, rx) = flume::bounded::<Ebuild>(jobs * 2);
    let repo = Arc::new(repo);
    let out_dir = Arc::new(out_dir);
    let errors = Arc::new(AtomicUsize::new(0));
    let eclass_cache: EclassChecksumCache = Arc::new(Mutex::new(HashMap::new()));

    let mut handles = Vec::new();
    for _ in 0..jobs {
        let rx = rx.clone();
        let repo = Arc::clone(&repo);
        let out_dir = Arc::clone(&out_dir);
        let errors = Arc::clone(&errors);
        let eclass_cache = Arc::clone(&eclass_cache);
        handles.push(tokio::spawn(async move {
            let masters: Vec<portage_repo::Repository> = vec![];
            while let Ok(ebuild) = rx.recv_async().await {
                if let Err(e) = process_ebuild(
                    &repo,
                    &masters,
                    &ebuild,
                    out_dir.as_ref().as_ref(),
                    &eclass_cache,
                )
                .await
                {
                    let cpv = ebuild.cpv();
                    eprintln!("ERROR {cpv}: {e}");
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    drop(rx);

    for ebuild in ebuilds {
        if tx.send(ebuild).is_err() {
            break;
        }
    }
    drop(tx);

    for h in handles {
        h.await.unwrap();
    }

    let err_count = errors.load(Ordering::Relaxed);
    println!("Total: {total}  Errors: {err_count}");

    if err_count > 0 {
        process::exit(1);
    }
}
