//! Minimal metadata regeneration benchmark — no comparison, optional write.
//!
//! Sources every ebuild in the repo in parallel and optionally writes the
//! resulting md5-cache files to a directory.  Intended as a like-for-like
//! comparison with:
//!
//!   pk repo metadata regen -p <cache-dir> -n -f -j <N> <repo>

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use brush_parser::ast::Program;
use clap::Parser;
use portage_metadata::CacheEntry;
use portage_repo::{Ebuild, Repository};

type EclassAstCache = Arc<papaya::HashMap<String, Program>>;

type EclassChecksumCache = Arc<Mutex<HashMap<PathBuf, md5::Digest>>>;

#[derive(Parser)]
#[command(about = "Source all ebuilds and optionally write an md5-cache")]
struct Args {
    /// Path to the repository
    repo: String,
    /// Optional category/package glob filter (e.g. 'dev-util/*')
    filter: Option<String>,
    /// Write cache files to this directory
    #[arg(short = 'o', long, value_name = "DIR")]
    output: Option<PathBuf>,
    /// Number of parallel workers (default: available CPUs)
    #[arg(short = 'j', long)]
    jobs: Option<usize>,
}

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
fn matches_filter(ebuild: &Ebuild, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let cat_end = filter.find('/').unwrap_or(filter.len());
    let filter_cat = &filter[..cat_end];
    if ebuild.category() != filter_cat {
        return false;
    }
    let rest = &filter[cat_end..];
    if rest == "/" || rest.ends_with("/*") && rest.len() == 2 {
        return true;
    }
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
    ast_cache: &EclassAstCache,
) -> Result<(), String> {
    let master_refs: Vec<&Repository> = masters.iter().collect();
    let mut shell = repo
        .shell_with_masters_and_cache(&master_refs, Arc::clone(ast_cache))
        .await
        .map_err(|e| format!("shell: {e}"))?;

    let metadata = shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| format!("source: {e}"))?;

    if let Some(dir) = out_dir {
        let ebuild_bytes = fs::read(ebuild.path()).map_err(|e| format!("read ebuild: {e}"))?;
        let ebuild_md5 = format!("{:x}", md5::compute(&ebuild_bytes));

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

    let args = Args::parse();

    let jobs = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });

    let repo = match Repository::open(&args.repo) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repo: {e}");
            process::exit(1);
        }
    };

    let ebuilds = match repo.ebuilds() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error listing ebuilds: {e}");
            process::exit(1);
        }
    };

    let ebuilds = if let Some(ref f) = args.filter {
        let f = f.clone();
        ebuilds.filter(move |eb| matches_filter(eb, &f)).collect_vec()
    } else {
        ebuilds.collect_vec()
    };

    let total = ebuilds.len();
    let filter_desc = args
        .filter
        .as_ref()
        .map(|f| format!(" (filter: {f})"))
        .unwrap_or_default();
    eprintln!(
        "Sourcing {total} ebuilds with {jobs} workers{}{}...",
        filter_desc,
        args.output
            .as_ref()
            .map(|p| format!(", writing to {}", p.display()))
            .unwrap_or_default()
    );

    let (tx, rx) = flume::bounded::<Ebuild>(jobs * 2);
    let repo = Arc::new(repo);
    let out_dir = Arc::new(args.output);
    let errors = Arc::new(AtomicUsize::new(0));
    let eclass_cache: EclassChecksumCache = Arc::new(Mutex::new(HashMap::new()));
    let ast_cache: EclassAstCache = Arc::new(papaya::HashMap::new());

    {
        let shell = repo
            .shell_with_masters_and_cache(&[], Arc::clone(&ast_cache))
            .await
            .expect("prewarm shell");
        shell.prewarm_eclass_cache();
        eprintln!("Prewarmed {} eclasses.", ast_cache.pin().len());
    }

    let mut handles = Vec::new();
    for _ in 0..jobs {
        let rx = rx.clone();
        let repo = Arc::clone(&repo);
        let out_dir = Arc::clone(&out_dir);
        let errors = Arc::clone(&errors);
        let eclass_cache = Arc::clone(&eclass_cache);
        let ast_cache = Arc::clone(&ast_cache);
        handles.push(tokio::spawn(async move {
            let masters: Vec<portage_repo::Repository> = vec![];
            while let Ok(ebuild) = rx.recv_async().await {
                if let Err(e) = process_ebuild(
                    &repo,
                    &masters,
                    &ebuild,
                    out_dir.as_ref().as_ref(),
                    &eclass_cache,
                    &ast_cache,
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
