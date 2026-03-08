//! Print a summary of a repository's contents: category / package / ebuild
//! counts, plus eclass and license totals.
//!
//! # Usage
//!
//! ```text
//! cargo run --example enumerate_repo -- [path/to/repo]
//! ```
//!
//! Defaults to `/var/db/repos/gentoo` when no path is given.

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

    println!("Repository: {}", repo.name());
    println!("Path: {}", repo.path().display());
    println!("Masters: {:?}", repo.layout().masters);
    println!();

    let categories = repo.categories().unwrap_or_default();
    println!("Categories: {}", categories.len());

    let mut total_packages = 0;
    let mut total_ebuilds = 0;

    for cat in &categories {
        if !cat.exists() {
            continue;
        }
        let packages = match cat.packages() {
            Ok(p) => p,
            Err(_) => continue,
        };
        for pkg in &packages {
            total_packages += 1;
            let ebuilds = match pkg.ebuilds() {
                Ok(e) => e,
                Err(_) => continue,
            };
            total_ebuilds += ebuilds.len();
        }
    }

    println!("Packages: {total_packages}");
    println!("Ebuilds: {total_ebuilds}");

    // Show eclasses
    if let Ok(eclasses) = repo.eclasses() {
        println!("Eclasses: {}", eclasses.len());
    }

    // Show licenses
    if let Ok(licenses) = repo.licenses() {
        println!("Licenses: {}", licenses.len());
    }

    // Show supported architectures
    let arches = repo.arch_list();
    if !arches.is_empty() {
        let keywords: Vec<&str> = arches.iter().map(|a| repo.arch_keyword(a)).collect();
        println!("Arches:   {} ({})", arches.len(), keywords.join(" "));
    }
}
