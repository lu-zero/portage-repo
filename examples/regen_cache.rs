use std::collections::BTreeMap;
use std::env;
use std::process;

use portage_metadata::CacheEntry;
use portage_repo::Repository;

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

struct Stats {
    total: usize,
    success: usize,
    errors: usize,
    mismatches: usize,
    missing_cache: usize,
}

struct FieldDiff {
    cpv: String,
    key: String,
    expected: String,
    got: String,
}

/// Regenerate metadata cache and compare against existing md5-cache.
///
/// Usage: regen_cache <repo-path> [filter] [--repos-dir <dir>]
///
/// If `--repos-dir` is given, master repositories listed in `layout.conf`
/// are resolved from that directory and their eclasses are available to
/// `inherit`.
///
/// Examples:
///   regen_cache gentoo
///   regen_cache gentoo 'dev-lang/*'
///   regen_cache /var/db/repos/my-overlay --repos-dir /var/db/repos
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "Usage: {} <repo-path> [filter] [--repos-dir <dir>]",
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

    // Parse optional --repos-dir and filter from remaining args.
    let mut filter: Option<&str> = None;
    let mut repos_dir: Option<&str> = None;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--repos-dir" {
            i += 1;
            if i < args.len() {
                repos_dir = Some(&args[i]);
            } else {
                eprintln!("--repos-dir requires an argument");
                process::exit(2);
            }
        } else if filter.is_none() {
            filter = Some(&args[i]);
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
    let categories = match repo.categories() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading categories: {e}");
            process::exit(1);
        }
    };

    eprintln!("Collecting ebuilds...");
    let mut ebuilds = Vec::new();
    for cat in &categories {
        let packages = match cat.packages() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Warning: skipping category {}: {e}", cat.name());
                continue;
            }
        };
        for pkg in &packages {
            let pkg_ebuilds = match pkg.ebuilds() {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "Warning: skipping package {}/{}: {e}",
                        cat.name(),
                        pkg.name()
                    );
                    continue;
                }
            };
            for ebuild in pkg_ebuilds {
                let cpv_str = ebuild.cpv().to_string();
                if let Some(f) = filter {
                    if !matches_filter(&cpv_str, f) {
                        continue;
                    }
                }
                ebuilds.push(ebuild);
            }
        }
    }

    let total = ebuilds.len();
    eprintln!("Found {total} ebuilds to process.");

    let mut stats = Stats {
        total,
        success: 0,
        errors: 0,
        mismatches: 0,
        missing_cache: 0,
    };
    let mut diffs: Vec<FieldDiff> = Vec::new();

    for (i, ebuild) in ebuilds.iter().enumerate() {
        let cpv = ebuild.cpv();
        let cpv_str = cpv.to_string();
        eprint!("\r[{}/{}] {}", i + 1, total, cpv_str);

        // Create a fresh shell for each ebuild (sourcing is not idempotent).
        let master_refs: Vec<&Repository> = masters.iter().collect();
        let mut shell = match repo.shell_with_masters(&master_refs).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("\nERROR creating shell for {cpv_str}: {e}");
                stats.errors += 1;
                continue;
            }
        };

        // Source the ebuild.
        let metadata = match shell.source_ebuild(ebuild).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("\nERROR sourcing {cpv_str}: {e}");
                stats.errors += 1;
                continue;
            }
        };

        // Read the reference cache entry.
        let reference = match repo.cache_entry(cpv) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("\nMISSING cache for {cpv_str}");
                stats.missing_cache += 1;
                // Still counts as "success" for sourcing — just can't compare.
                stats.success += 1;
                continue;
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
            if ref_val != src_val {
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
    }

    // Clear the progress line.
    eprintln!();

    // Print diff summary.
    if !diffs.is_empty() {
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
