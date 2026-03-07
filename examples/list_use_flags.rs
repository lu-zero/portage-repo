//! List all USE flags and their descriptions from a repository.
//!
//! Usage:
//!   cargo run --example list_use_flags -- [path/to/repo]
//!
//! Prints three sections:
//!   1. Global USE flags (profiles/use.desc)
//!   2. USE_EXPAND groups and their values (profiles/desc/*.desc)
//!   3. Per-package USE flags collected from every metadata.xml

use std::collections::BTreeMap;
use std::env;

use portage_repo::Repository;

fn main() {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "/var/db/repos/gentoo".to_string());

    let repo = match Repository::open(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repository at {path}: {e}");
            std::process::exit(1);
        }
    };

    // ── 1. Global USE flags ──────────────────────────────────────────────────
    println!("=== Global USE flags (profiles/use.desc) ===");
    match repo.use_desc() {
        Ok(flags) => {
            // use_desc returns pairs in file order; sort alphabetically.
            let mut sorted: Vec<_> = flags.into_iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            for (flag, desc) in &sorted {
                println!("  {flag:<30} {desc}");
            }
            println!("  ({} global flags)\n", sorted.len());
        }
        Err(e) => eprintln!("  warning: {e}\n"),
    }

    // ── 2. USE_EXPAND groups ─────────────────────────────────────────────────
    println!("=== USE_EXPAND groups (profiles/desc/*.desc) ===");
    match repo.use_expand_names() {
        Ok(groups) => {
            for group in &groups {
                match repo.use_expand_desc(group) {
                    Ok(values) => {
                        println!("  [{group}] ({} values)", values.len());
                        for (val, desc) in &values {
                            let flag = format!("{group}_{val}");
                            println!("    {flag:<40} {desc}");
                        }
                    }
                    Err(e) => eprintln!("  warning: {group}: {e}"),
                }
            }
            println!("  ({} groups)\n", groups.len());
        }
        Err(e) => eprintln!("  warning: {e}\n"),
    }

    // ── 3. Per-package USE flags from metadata.xml ───────────────────────────
    println!("=== Per-package USE flags (metadata.xml) ===");

    // Collect into BTreeMap<cpn_string, BTreeMap<flag, desc>> so output is sorted.
    let mut pkg_flags: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut xml_errors = 0usize;

    let categories = repo.categories().unwrap_or_default();
    for cat in &categories {
        if !cat.exists() {
            continue;
        }
        let packages = match cat.packages() {
            Ok(p) => p,
            Err(_) => continue,
        };
        for pkg in &packages {
            if !pkg.has_metadata_xml() {
                continue;
            }
            match pkg.metadata_xml() {
                Ok(Some(meta)) if !meta.use_flags.is_empty() => {
                    let sorted: BTreeMap<_, _> = meta.use_flags.into_iter().collect();
                    pkg_flags.insert(pkg.cpn().to_string(), sorted);
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("  warning: {}: {e}", pkg.cpn());
                    xml_errors += 1;
                }
            }
        }
    }

    let expand = repo.use_expand().unwrap_or_default();

    let total_pkg_flags: usize = pkg_flags.values().map(|m| m.len()).sum();
    for (cpn, flags) in &pkg_flags {
        println!("  [{cpn}]");
        // Group flags by USE_EXPAND prefix; truly global flags are shown flat.
        let mut groups: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
        for (flag, desc) in flags {
            let (group, value) = expand.split(flag);
            groups.entry(group).or_default().push((value, desc.as_str()));
        }
        for (group, values) in &groups {
            if *group == "global" {
                for (flag, desc) in values {
                    println!("    {flag:<30} {desc}");
                }
            } else {
                println!("    [{group}]");
                for (value, desc) in values {
                    println!("      {value:<28} {desc}");
                }
            }
        }
    }
    if xml_errors > 0 {
        eprintln!("  ({xml_errors} metadata.xml parse errors)");
    }
    println!("  ({} packages, {total_pkg_flags} per-package flags)", pkg_flags.len());
}
