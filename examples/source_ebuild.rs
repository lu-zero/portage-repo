//! Source a single ebuild through the embedded bash shell and print the
//! extracted PMS metadata variables.
//!
//! # Usage
//!
//! ```text
//! cargo run --example source_ebuild -- <repo-path> <category/package-version>
//! ```
//!
//! # Example
//!
//! ```text
//! cargo run --example source_ebuild -- gentoo dev-lang/rust-1.75.0
//! ```

use std::env;
use std::process;

use portage_repo::Repository;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <repo-path> <category/package-version>", args[0]);
        eprintln!(
            "Example: {} /var/db/repos/gentoo dev-lang/rust-1.75.0",
            args[0]
        );
        process::exit(2);
    }
    let repo_path = &args[1];
    let cpv_str = &args[2];

    let repo = match Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repository: {e}");
            process::exit(1);
        }
    };

    // Parse the cpv to locate the ebuild
    let cpv = match portage_atom::Cpv::parse(cpv_str) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Invalid atom {cpv_str}: {e}");
            process::exit(1);
        }
    };

    let category = match repo.category(cpv.category()) {
        Some(c) => c,
        None => {
            eprintln!("Category {} not found", cpv.category());
            process::exit(1);
        }
    };

    let package = match category.package(cpv.package()) {
        Some(p) => p,
        None => {
            eprintln!("Package {} not found in {}", cpv.package(), cpv.category());
            process::exit(1);
        }
    };

    let version_str = cpv.version.to_string();
    let ebuild = match package.ebuild(&version_str) {
        Ok(Some(e)) => e,
        Ok(None) => {
            eprintln!("Ebuild {cpv_str} not found");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Error looking up ebuild: {e}");
            process::exit(1);
        }
    };

    println!("Sourcing {}", ebuild.path().display());
    println!();

    // Create the shell and source the ebuild
    let mut shell = match repo.shell().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error creating shell: {e}");
            process::exit(1);
        }
    };

    let metadata = match shell.source_ebuild(&ebuild).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Error sourcing ebuild: {e}");
            process::exit(1);
        }
    };

    // Print extracted metadata
    println!("EAPI:         {}", metadata.eapi);
    println!("DESCRIPTION:  {}", metadata.description);
    println!("SLOT:         {}", metadata.slot);
    println!("HOMEPAGE:     {:?}", metadata.homepage);
    println!("KEYWORDS:     {:?}", metadata.keywords);
    println!("IUSE:         {:?}", metadata.iuse);
    println!("LICENSE:      {:?}", metadata.license);

    if !metadata.depend.is_empty() {
        println!("DEPEND:       {:?}", metadata.depend);
    }
    if !metadata.rdepend.is_empty() {
        println!("RDEPEND:      {:?}", metadata.rdepend);
    }
    if !metadata.bdepend.is_empty() {
        println!("BDEPEND:      {:?}", metadata.bdepend);
    }
    if !metadata.pdepend.is_empty() {
        println!("PDEPEND:      {:?}", metadata.pdepend);
    }
    if !metadata.idepend.is_empty() {
        println!("IDEPEND:      {:?}", metadata.idepend);
    }
    if !metadata.restrict.is_empty() {
        println!("RESTRICT:     {:?}", metadata.restrict);
    }
    if !metadata.properties.is_empty() {
        println!("PROPERTIES:   {:?}", metadata.properties);
    }
    if metadata.required_use.is_some() {
        println!("REQUIRED_USE: {:?}", metadata.required_use);
    }
    if !metadata.inherited.is_empty() {
        println!("INHERITED:    {:?}", metadata.inherited);
    }
    if !metadata.defined_phases.is_empty() {
        println!("PHASES:       {:?}", metadata.defined_phases);
    }
}
