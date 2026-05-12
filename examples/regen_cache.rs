//! Source every ebuild in a repository and compare the extracted metadata
//! against the existing `metadata/md5-cache/` entries.
//!
//! Progress is written to stderr; the final stats table goes to stdout.
//! Exit code is 1 if there are any sourcing errors or metadata mismatches.

use std::collections::BTreeMap;
use std::fs;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::Parser;
use portage_metadata::CacheEntry;
use portage_repo::{Ebuild, Repository};

/// Fields to compare between sourced metadata and the md5-cache.
///
/// Note: `INHERITED` (transitive eclass list) is intentionally excluded — it
/// is stored in the md5-cache as `_eclasses_=` with checksums and is not a
/// directly comparable text field.  `INHERIT` (direct eclass list) is included
/// because both sides now produce it correctly.
const COMPARE_KEYS: &[&str] = &[
    "EAPI",
    "DESCRIPTION",
    "SLOT",
    "HOMEPAGE",
    "SRC_URI",
    "LICENSE",
    "KEYWORDS",
    "IUSE",
    "REQUIRED_USE",
    "RESTRICT",
    "PROPERTIES",
    "DEPEND",
    "RDEPEND",
    "BDEPEND",
    "PDEPEND",
    "IDEPEND",
    "DEFINED_PHASES",
    "INHERIT",
];

/// Fields where token order does not affect semantic equivalence.
const UNORDERED_KEYS: &[&str] = &[
    "SRC_URI",
    "LICENSE",
    "IUSE",
    "KEYWORDS",
    "REQUIRED_USE",
    "RESTRICT",
    "PROPERTIES",
    "DEPEND",
    "RDEPEND",
    "BDEPEND",
    "PDEPEND",
    "IDEPEND",
];

const STRUCTURAL_TOKENS: &[&str] = &["(", ")", "||", "&&"];

fn token_multiset<'a>(s: &'a str) -> BTreeMap<&'a str, usize> {
    let mut map = BTreeMap::new();
    for tok in s.split_whitespace() {
        *map.entry(tok).or_insert(0) += 1;
    }
    map
}

fn find_extra_duplicates<'a>(ref_val: &'a str, src: &'a str) -> Vec<String> {
    let mut ref_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for tok in ref_val.split_whitespace() {
        if !STRUCTURAL_TOKENS.contains(&tok) {
            *ref_counts.entry(tok).or_insert(0) += 1;
        }
    }
    let mut src_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for tok in src.split_whitespace() {
        if !STRUCTURAL_TOKENS.contains(&tok) {
            *src_counts.entry(tok).or_insert(0) += 1;
        }
    }
    src_counts
        .into_iter()
        .filter(|(tok, src_n)| *src_n > *ref_counts.get(tok).unwrap_or(&0))
        .map(|(tok, _)| tok.to_string())
        .collect()
}

fn parse_cache_map(serialized: &str) -> BTreeMap<&str, &str> {
    let mut map = BTreeMap::new();
    for line in serialized.lines() {
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key, value);
        }
    }
    map
}

fn matches_filter(cpv: &str, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    if let Some(prefix) = filter.strip_suffix('*') {
        cpv.starts_with(prefix)
    } else {
        cpv == filter
    }
}

#[derive(Default)]
struct Stats {
    total: usize,
    success: usize,
    errors: usize,
    mismatches: usize,
    missing_cache: usize,
}

impl Stats {
    fn merge(&mut self, other: Stats) {
        self.total += other.total;
        self.success += other.success;
        self.errors += other.errors;
        self.mismatches += other.mismatches;
        self.missing_cache += other.missing_cache;
    }
}

struct FieldDiff {
    cpv: String,
    key: String,
    expected: String,
    got: String,
}

async fn process_ebuild(
    repo: &Repository,
    masters: &[Repository],
    ebuild: &Ebuild,
    progress: &AtomicUsize,
    total: usize,
    quiet: bool,
    eclass_cache: &Arc<papaya::HashMap<String, brush_parser::ast::Program>>,
) -> (Stats, Vec<FieldDiff>) {
    let mut stats = Stats::default();
    let mut diffs = Vec::new();
    stats.total = 1;

    let cpv = ebuild.cpv();
    let cpv_str = cpv.to_string();
    let i = progress.fetch_add(1, Ordering::Relaxed) + 1;
    if !quiet {
        eprint!("\r[{i}/{total}] {cpv_str:<60}");
    }

    let master_refs: Vec<&Repository> = masters.iter().collect();
    let mut shell = match repo
        .shell_with_masters_and_cache(&master_refs, eclass_cache.clone())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\nERROR creating shell for {cpv_str}: {e}");
            stats.errors += 1;
            return (stats, diffs);
        }
    };

    let metadata = match shell.source_ebuild(ebuild).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("\nERROR sourcing {cpv_str}: {e}");
            stats.errors += 1;
            return (stats, diffs);
        }
    };

    let reference = match repo.cache_entry(cpv) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("\nMISSING cache for {cpv_str}");
            stats.missing_cache += 1;
            stats.success += 1;
            return (stats, diffs);
        }
    };

    let ebuild_md5 = fs::read(ebuild.path())
        .map(|b| format!("{:x}", md5::compute(&b)))
        .ok();

    let mut eclasses = Vec::new();
    for name in &metadata.inherited {
        if let Some(path) = shell.eclass_path(name) {
            if let Ok(data) = fs::read(&path) {
                eclasses.push((name.clone(), format!("{:x}", md5::compute(&data))));
            }
        }
    }

    let sourced_entry = CacheEntry {
        metadata,
        md5: ebuild_md5,
        eclasses,
    };

    let ref_serialized = reference.serialize();
    let src_serialized = sourced_entry.serialize();

    let ref_map = parse_cache_map(&ref_serialized);
    let src_map = parse_cache_map(&src_serialized);

    let mut has_diff = false;
    for &key in COMPARE_KEYS {
        let ref_val = ref_map.get(key).copied().unwrap_or("");
        let src_val = src_map.get(key).copied().unwrap_or("");

        if UNORDERED_KEYS.contains(&key) && !src_val.is_empty() {
            let dups = find_extra_duplicates(ref_val, src_val);
            if !dups.is_empty() {
                eprintln!(
                    "\nWARN {cpv_str} {key}: extra duplicate tokens (not in reference): {}",
                    dups.join(", ")
                );
            }
        }

        let differs = if UNORDERED_KEYS.contains(&key) {
            token_multiset(ref_val) != token_multiset(src_val)
        } else {
            ref_val != src_val
        };

        if differs {
            has_diff = true;
            diffs.push(FieldDiff {
                cpv: cpv_str.clone(),
                key: key.to_string(),
                expected: ref_val.to_string(),
                got: src_val.to_string(),
            });
        }
    }

    if has_diff {
        stats.mismatches += 1;
    }
    stats.success += 1;
    (stats, diffs)
}

#[derive(Parser)]
#[command(about = "Source all ebuilds and compare against the md5-cache")]
struct Args {
    /// Path to the repository
    repo: String,
    /// Optional category/package glob filter (e.g. 'dev-lang/*')
    filter: Option<String>,
    /// Directory containing master repositories
    #[arg(long, value_name = "DIR")]
    repos_dir: Option<String>,
    /// Number of parallel workers (default: available CPUs)
    #[arg(short = 'j', long)]
    jobs: Option<usize>,
    /// Suppress per-ebuild progress output
    #[arg(short, long)]
    quiet: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let jobs = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });

    let (repo, masters) = if let Some(ref dir) = args.repos_dir {
        match Repository::open_with_masters(&args.repo, dir) {
            Ok((r, m)) => {
                if !m.is_empty() {
                    let names: Vec<&str> = m.iter().map(|r| r.name()).collect();
                    eprintln!("Resolved masters: {}", names.join(", "));
                }
                (r, m)
            }
            Err(e) => {
                eprintln!("Error opening repository with masters: {e}");
                process::exit(1);
            }
        }
    } else {
        match Repository::open(&args.repo) {
            Ok(r) => (r, Vec::new()),
            Err(e) => {
                eprintln!("Error opening repository: {e}");
                process::exit(1);
            }
        }
    };

    eprintln!("Collecting ebuilds...");
    let ebuilds = match repo.ebuilds() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error collecting ebuilds: {e}");
            process::exit(1);
        }
    };

    let ebuilds = if let Some(ref f) = args.filter {
        let f = f.clone();
        ebuilds.filter(move |eb| matches_filter(&eb.cpv().to_string(), &f)).collect_vec()
    } else {
        ebuilds.collect_vec()
    };

    let total = ebuilds.len();
    eprintln!("Found {total} ebuilds to process with {jobs} workers.");

    let (tx, rx) = flume::bounded::<Ebuild>(jobs * 2);
    let repo = Arc::new(repo);
    let masters = Arc::new(masters);
    let progress = Arc::new(AtomicUsize::new(0));
    let eclass_cache: Arc<papaya::HashMap<String, brush_parser::ast::Program>> =
        Arc::new(papaya::HashMap::new());

    {
        let master_refs: Vec<&Repository> = masters.iter().collect();
        let shell = repo
            .shell_with_masters_and_cache(&master_refs, Arc::clone(&eclass_cache))
            .await
            .expect("prewarm shell");
        shell.prewarm_eclass_cache();
        if !args.quiet {
            eprintln!("Prewarmed {} eclasses.", eclass_cache.pin().len());
        }
    }

    let mut handles = Vec::new();
    for _ in 0..jobs {
        let rx = rx.clone();
        let repo = Arc::clone(&repo);
        let masters = Arc::clone(&masters);
        let progress = Arc::clone(&progress);
        let eclass_cache = Arc::clone(&eclass_cache);
        let quiet = args.quiet;
        handles.push(tokio::spawn(async move {
            let mut stats = Stats::default();
            let mut diffs = Vec::new();
            while let Ok(ebuild) = rx.recv_async().await {
                let (s, d) = process_ebuild(
                    &repo,
                    &masters,
                    &ebuild,
                    &progress,
                    total,
                    quiet,
                    &eclass_cache,
                )
                .await;
                stats.merge(s);
                diffs.extend(d);
            }
            (stats, diffs)
        }));
    }
    drop(rx);

    for ebuild in ebuilds {
        if tx.send(ebuild).is_err() {
            break;
        }
    }
    drop(tx);

    let mut stats = Stats::default();
    stats.total = total;
    let mut diffs = Vec::new();
    for handle in handles {
        let (s, d) = handle.await.unwrap();
        stats.success += s.success;
        stats.errors += s.errors;
        stats.mismatches += s.mismatches;
        stats.missing_cache += s.missing_cache;
        diffs.extend(d);
    }

    if !args.quiet {
        eprintln!();
    }

    if !diffs.is_empty() {
        diffs.sort_by(|a, b| a.cpv.cmp(&b.cpv).then(a.key.cmp(&b.key)));
        eprintln!("=== Field diffs ===");
        for d in &diffs {
            eprintln!("DIFF {} {}:", d.cpv, d.key);
            eprintln!("  cache: {}", d.expected);
            eprintln!("  got:   {}", d.got);
        }
        eprintln!();
    }

    println!("=== Results ===");
    println!("Total:         {}", stats.total);
    println!("Sourced OK:    {}", stats.success);
    println!("Errors:        {}", stats.errors);
    println!("Mismatches:    {}", stats.mismatches);
    println!("Missing cache: {}", stats.missing_cache);

    let (hits, misses) = portage_repo::inherit::cache_stats();
    let total_lookups = hits + misses;
    if total_lookups > 0 {
        println!(
            "Eclass cache:  {} hits / {} misses ({:.1}% hit rate)",
            hits,
            misses,
            hits as f64 / total_lookups as f64 * 100.0
        );
    }

    if stats.errors > 0 || stats.mismatches > 0 {
        process::exit(1);
    }
}
