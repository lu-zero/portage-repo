//! List and inspect profiles from a repository.
//!
//! # Usage
//!
//! ```
//! # List all profiles, grouped by architecture
//! cargo run --example list_profiles -- gentoo
//!
//! # Filter to one architecture
//! cargo run --example list_profiles -- gentoo amd64
//!
//! # Full detail for a specific profile (path contains '/')
//! cargo run --example list_profiles -- gentoo default/linux/amd64/23.0
//! ```
//!
//! When a profile path is given the stack is resolved and `make.defaults`
//! is sourced through the embedded shell so the fully-expanded USE flag
//! list (after force/mask) is shown.

use std::env;
use std::process;

use portage_repo::{ProfileStatus, Repository};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <repo-path> [arch | profile/path]", args[0]);
        process::exit(2);
    }
    let repo_path = &args[1];
    let filter = args.get(2).map(String::as_str);

    let repo = match Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repository: {e}");
            process::exit(1);
        }
    };

    let all_profiles = match repo.profiles_desc() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error reading profiles.desc: {e}");
            process::exit(1);
        }
    };

    // If the filter contains '/' it is a profile path — run full inspect.
    if let Some(path) = filter.filter(|s| s.contains('/')) {
        inspect_profile(&repo, path).await;
        return;
    }

    // Otherwise: list profiles, optionally filtered by arch.
    let profiles: Vec<_> = all_profiles
        .iter()
        .filter(|p| filter.is_none_or(|arch| p.arch == arch))
        .collect();

    if profiles.is_empty() {
        eprintln!("No profiles found for arch {:?}", filter.unwrap_or("(any)"));
        process::exit(1);
    }

    // Group by arch for display.
    let mut current_arch = String::new();
    for desc in &profiles {
        if desc.arch != current_arch {
            println!("\n[{}]", desc.arch);
            current_arch = desc.arch.clone();
        }

        let status = match &desc.status {
            ProfileStatus::Stable => "stable",
            ProfileStatus::Dev => "dev",
            ProfileStatus::Exp => "exp",
            ProfileStatus::Other(s) => s.as_str(),
        };

        // Resolve the stack to get depth and basic stats (no shell needed).
        match repo.profile_stack(&desc.path) {
            Ok(stack) => {
                let depth = stack.profiles().len();
                let deprecated = if stack.is_deprecated() { " [DEPRECATED]" } else { "" };
                let force = stack.use_force().map(|v| v.len()).unwrap_or(0);
                let mask = stack.use_mask().map(|v| v.len()).unwrap_or(0);
                let pkg_mask = stack.package_mask().map(|v| v.len()).unwrap_or(0);
                let sys_pkgs = stack
                    .packages()
                    .map(|v| v.iter().filter(|(sys, _)| *sys).count())
                    .unwrap_or(0);
                println!(
                    "  {:<45} {:6}  depth={depth}  force={force}  mask={mask}  \
                     pkg_mask={pkg_mask}  sys={sys_pkgs}{deprecated}",
                    desc.path, status,
                );
            }
            Err(e) => {
                println!("  {:<45} {:6}  (stack error: {e})", desc.path, status);
            }
        }
    }
    println!();
}

/// Show full detail for a single profile, including resolved USE flags.
async fn inspect_profile(repo: &Repository, profile_path: &str) {
    let stack = match repo.profile_stack(profile_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error building profile stack: {e}");
            process::exit(1);
        }
    };

    println!("Profile:    {profile_path}");
    println!("Deprecated: {}", stack.is_deprecated());
    println!();

    // ── Inheritance chain ─────────────────────────────────────────────────
    println!("=== Inheritance chain ({} profiles) ===", stack.profiles().len());
    for (i, p) in stack.profiles().iter().enumerate() {
        println!("  [{i}] {}", p.path().display());
    }
    println!();

    // ── use.force / use.mask ──────────────────────────────────────────────
    if let Ok(force) = stack.use_force() {
        if !force.is_empty() {
            let mut sorted = force.clone();
            sorted.sort();
            println!("=== use.force ({} flags) ===", sorted.len());
            print_wrapped(&sorted, 6);
        }
    }
    if let Ok(mask) = stack.use_mask() {
        if !mask.is_empty() {
            let mut sorted = mask.clone();
            sorted.sort();
            println!("=== use.mask ({} flags) ===", sorted.len());
            print_wrapped(&sorted, 6);
        }
    }
    if let Ok(sf) = stack.use_stable_force() {
        if !sf.is_empty() {
            println!("=== use.stable.force ({} flags) ===", sf.len());
            print_wrapped(&sf, 6);
        }
    }
    if let Ok(sm) = stack.use_stable_mask() {
        if !sm.is_empty() {
            println!("=== use.stable.mask ({} flags) ===", sm.len());
            print_wrapped(&sm, 6);
        }
    }

    // ── System packages ───────────────────────────────────────────────────
    if let Ok(pkgs) = stack.packages() {
        let sys: Vec<_> = pkgs.iter().filter(|(s, _)| *s).map(|(_, d)| d).collect();
        if !sys.is_empty() {
            println!("=== System packages ({}) ===", sys.len());
            for dep in &sys {
                println!("  {dep}");
            }
            println!();
        }
    }

    // ── Package masks ─────────────────────────────────────────────────────
    if let Ok(masks) = stack.package_mask() {
        if !masks.is_empty() {
            println!("=== package.mask ({} atoms) ===", masks.len());
            for dep in masks.iter().take(20) {
                println!("  {dep}");
            }
            if masks.len() > 20 {
                println!("  ... ({} more)", masks.len() - 20);
            }
            println!();
        }
    }

    // ── Resolved USE flags (requires shell + make.defaults) ───────────────
    println!("=== Resolved USE flags (after make.defaults + force/mask) ===");
    let mut shell = match repo.shell().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error creating shell: {e}");
            process::exit(1);
        }
    };
    match stack.configure_shell(&mut shell, &[]).await {
        Ok(()) => {
            let flags: Vec<String> = shell
                .use_flags_string()
                .split_whitespace()
                .map(str::to_string)
                .collect();

            // Build prefix table from $USE_EXPAND: group name → lowercase prefix.
            // Sort longest-prefix-first so e.g. "cpu_flags_x86" beats "cpu_flags".
            let mut prefixes: Vec<(String, String)> = shell
                .get_var("USE_EXPAND")
                .unwrap_or_default()
                .split_whitespace()
                .map(|g| (g.to_lowercase(), g.to_lowercase()))
                .collect();
            prefixes.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

            // Bucket each flag: find the first (longest) matching USE_EXPAND prefix.
            let mut groups: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for flag in &flags {
                let bucket = prefixes
                    .iter()
                    .find(|(prefix, _)| flag.starts_with(&format!("{prefix}_")))
                    .map(|(_, group)| group.as_str())
                    .unwrap_or("global");
                let value = if bucket == "global" {
                    flag.clone()
                } else {
                    flag[bucket.len() + 1..].to_string() // strip "group_" prefix
                };
                groups.entry(bucket.to_string()).or_default().push(value);
            }

            println!("  ({} flags across {} groups)", flags.len(), groups.len());
            println!();
            for (group, mut values) in groups {
                values.sort();
                print!("  [{group}]");
                let header_len = group.len() + 4; // "  [group]".len()
                let indent = " ".repeat(header_len);
                let max_width = 100;
                let mut line = String::new();
                for value in &values {
                    if line.len() + value.len() + 1 > max_width - header_len
                        && !line.is_empty()
                    {
                        println!("  {line}");
                        line = format!("{indent}{value}");
                    } else {
                        if !line.is_empty() {
                            line.push(' ');
                        }
                        line.push_str(value);
                    }
                }
                if !line.is_empty() {
                    println!("  {line}");
                }
            }
            println!();
        }
        Err(e) => eprintln!("  Error resolving USE flags: {e}"),
    }
}

/// Print a list of strings wrapped at 100 columns with the given indent.
fn print_wrapped(items: &[String], indent: usize) {
    let indent_str = " ".repeat(indent);
    let max_width = 100;
    let mut line = indent_str.clone();
    for item in items {
        if line.len() + item.len() + 1 > max_width && !line.trim().is_empty() {
            println!("{line}");
            line = indent_str.clone();
        }
        if !line.trim().is_empty() {
            line.push(' ');
        }
        line.push_str(item);
    }
    if !line.trim().is_empty() {
        println!("{line}");
    }
    println!();
}
