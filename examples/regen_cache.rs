use std::collections::BTreeMap;
use std::env;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use portage_metadata::CacheEntry;
use portage_repo::{Ebuild, Repository};

/// Fields to compare between sourced metadata and the md5-cache.
///
/// Note: `INHERITED` is intentionally excluded — the md5-cache format does
/// not store it (it uses `_eclasses_` with checksums instead), so the
/// reference value is always empty and comparison is meaningless.
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
];

/// Fields where token order does not affect semantic equivalence.
///
/// For these fields the comparison ignores ordering: two values are considered
/// equal iff they contain the same tokens with the same frequencies (multiset
/// equality).  Portage does not guarantee a stable ordering for dep specs and
/// USE flags, so a pure string comparison would produce spurious diffs.
///
/// SRC_URI and LICENSE are also included: ebuilds often build these by
/// iterating associative-array keys/values whose traversal order is
/// implementation-defined (bash's hash order vs. brush's order differ).
/// Portage itself treats both fields as unordered sets at install time.
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

/// Dep-spec structural tokens that may legitimately appear multiple times.
const STRUCTURAL_TOKENS: &[&str] = &["(", ")", "||", "&&"];

/// Build a token → count map for a whitespace-separated string.
fn token_multiset<'a>(s: &'a str) -> BTreeMap<&'a str, usize> {
    let mut map = BTreeMap::new();
    for tok in s.split_whitespace() {
        *map.entry(tok).or_insert(0) += 1;
    }
    map
}

/// Return non-structural tokens that appear more than once in `s`.
fn find_duplicates(s: &str) -> Vec<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for tok in s.split_whitespace() {
        if !STRUCTURAL_TOKENS.contains(&tok) {
            *counts.entry(tok).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(tok, _)| tok.to_string())
        .collect()
}

/// Parse a serialized cache string into a KEY→value map.
fn parse_cache_map(serialized: &str) -> BTreeMap<&str, &str> {
    let mut map = BTreeMap::new();
    for line in serialized.lines() {
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key, value);
        }
    }
    map
}

/// Check whether a CPV string matches a glob-like filter.
///
/// Supports patterns like:
/// - `dev-lang/*`       — all ebuilds in dev-lang
/// - `dev-lang/rust-*`  — all rust versions in dev-lang
/// - `dev-lang/rust-1.88.0` — exact match
fn matches_filter(cpv: &str, filter: &str) -> bool {
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

/// Process a single ebuild: create shell, source, compare against cache.
async fn process_ebuild(
    repo: &Repository,
    masters: &[Repository],
    ebuild: &Ebuild,
    progress: &AtomicUsize,
    total: usize,
) -> (Stats, Vec<FieldDiff>) {
    let mut stats = Stats::default();
    let mut diffs = Vec::new();
    stats.total = 1;

    let cpv = ebuild.cpv();
    let cpv_str = cpv.to_string();
    let i = progress.fetch_add(1, Ordering::Relaxed) + 1;
    eprint!("\r[{i}/{total}] {cpv_str:<60}");

    // Create a fresh shell for each ebuild (sourcing is not idempotent).
    let master_refs: Vec<&Repository> = masters.iter().collect();
    let mut shell = match repo.shell_with_masters(&master_refs).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\nERROR creating shell for {cpv_str}: {e}");
            stats.errors += 1;
            return (stats, diffs);
        }
    };

    // Source the ebuild.
    let metadata = match shell.source_ebuild(ebuild).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("\nERROR sourcing {cpv_str}: {e}");
            stats.errors += 1;
            return (stats, diffs);
        }
    };

    // Read the reference cache entry.
    let reference = match repo.cache_entry(cpv) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("\nMISSING cache for {cpv_str}");
            stats.missing_cache += 1;
            stats.success += 1;
            return (stats, diffs);
        }
    };

    // Build a CacheEntry from the sourced metadata and serialize both.
    let sourced_entry = CacheEntry {
        metadata,
        md5: None,
        eclasses: vec![],
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
            let dups = find_duplicates(src_val);
            if !dups.is_empty() {
                eprintln!(
                    "\nWARN {cpv_str} {key}: duplicate tokens: {}",
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

/// Regenerate metadata cache and compare against existing md5-cache.
///
/// Usage: regen_cache <repo-path> [filter] [--repos-dir <dir>] [--jobs <N>]
///
/// If `--repos-dir` is given, master repositories listed in `layout.conf`
/// are resolved from that directory and their eclasses are available to
/// `inherit`.
///
/// Examples:
///   regen_cache gentoo
///   regen_cache gentoo 'dev-lang/*'
///   regen_cache /var/db/repos/my-overlay --repos-dir /var/db/repos
///   regen_cache gentoo --jobs 8
#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "Usage: {} <repo-path> [filter] [--repos-dir <dir>] [--jobs <N>]",
            args[0]
        );
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  {} gentoo", args[0]);
        eprintln!("  {} gentoo 'dev-lang/*'", args[0]);
        eprintln!(
            "  {} /var/db/repos/my-overlay --repos-dir /var/db/repos",
            args[0]
        );
        process::exit(2);
    }
    let repo_path = &args[1];

    // Parse optional --repos-dir, --jobs, and filter from remaining args.
    let mut filter: Option<String> = None;
    let mut repos_dir: Option<&str> = None;
    let mut jobs: usize = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--repos-dir" => {
                i += 1;
                if i < args.len() {
                    repos_dir = Some(&args[i]);
                } else {
                    eprintln!("--repos-dir requires an argument");
                    process::exit(2);
                }
            }
            "--jobs" => {
                i += 1;
                if i < args.len() {
                    jobs = args[i].parse().unwrap_or_else(|_| {
                        eprintln!("--jobs requires a number");
                        process::exit(2);
                    });
                } else {
                    eprintln!("--jobs requires an argument");
                    process::exit(2);
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

    let (repo, masters) = if let Some(dir) = repos_dir {
        match Repository::open_with_masters(repo_path, dir) {
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
        match Repository::open(repo_path) {
            Ok(r) => (r, Vec::new()),
            Err(e) => {
                eprintln!("Error opening repository: {e}");
                process::exit(1);
            }
        }
    };

    // Collect all ebuilds (with optional filtering) so we know the total count.
    eprintln!("Collecting ebuilds...");
    let mut ebuilds = match repo.ebuilds() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error collecting ebuilds: {e}");
            process::exit(1);
        }
    };

    if let Some(ref f) = filter {
        ebuilds.retain(|eb| matches_filter(&eb.cpv().to_string(), f));
    }

    let total = ebuilds.len();
    eprintln!("Found {total} ebuilds to process with {jobs} workers.");

    // Feed ebuilds to async workers via flume.
    let (tx, rx) = flume::bounded::<Ebuild>(jobs * 2);
    let repo = Arc::new(repo);
    let masters = Arc::new(masters);
    let progress = Arc::new(AtomicUsize::new(0));

    // Spawn worker tasks.
    let mut handles = Vec::new();
    for _ in 0..jobs {
        let rx = rx.clone();
        let repo = Arc::clone(&repo);
        let masters = Arc::clone(&masters);
        let progress = Arc::clone(&progress);
        handles.push(tokio::spawn(async move {
            let mut stats = Stats::default();
            let mut diffs = Vec::new();
            while let Ok(ebuild) = rx.recv_async().await {
                let (s, d) = process_ebuild(&repo, &masters, &ebuild, &progress, total).await;
                stats.merge(s);
                diffs.extend(d);
            }
            (stats, diffs)
        }));
    }
    // No more receivers needed in main — drop so workers exit when queue drains.
    drop(rx);

    // Send ebuilds from the collected vec.
    for ebuild in ebuilds {
        if tx.send(ebuild).is_err() {
            break; // all workers gone
        }
    }
    drop(tx);

    // Collect results.
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

    // Clear the progress line.
    eprintln!();

    // Print diff summary.
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

    // Final stats to stdout.
    println!("=== Results ===");
    println!("Total:         {}", stats.total);
    println!("Sourced OK:    {}", stats.success);
    println!("Errors:        {}", stats.errors);
    println!("Mismatches:    {}", stats.mismatches);
    println!("Missing cache: {}", stats.missing_cache);

    if stats.errors > 0 || stats.mismatches > 0 {
        process::exit(1);
    }
}
