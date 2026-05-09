//! Run a single ebuild phase — similar to `portage ebuild <file> <phase>`.
//!
//! Sources the ebuild through the embedded bash shell, then executes the
//! requested phase function.  Build directories are created under a temporary
//! location (or a path you specify with `--work-dir`).
//!
//! # Usage
//!
//! ```text
//! cargo run --example ebuild -- <repo-path> <category/package-version> <phase> [options]
//! ```
//!
//! # Phases
//!
//! pretend, setup, unpack, prepare, configure, compile, test, install,
//! preinst, postinst, prerm, postrm, nofetch, info, config
//!
//! # Options
//!
//! ```text
//! --use flag1 flag2 …   Set active USE flags (default: none)
//! --work-dir <path>     Use this directory for WORKDIR/T/D (default: /tmp/portage/<cpv>)
//! ```
//!
//! # Examples
//!
//! ```text
//! cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.1 compile
//! cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.1 install --work-dir /tmp/hello-build
//! cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.1 setup --use nls
//! ```

use std::env;
use std::path::PathBuf;
use std::process;

use portage_atom::Cpv;
use portage_repo::Repository;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "Usage: {} <repo-path> <category/package-version> <phase> [--use flags...] [--work-dir path]",
            args[0]
        );
        eprintln!();
        eprintln!("Phases: pretend setup unpack prepare configure compile test install");
        eprintln!("        preinst postinst prerm postrm nofetch info config");
        process::exit(2);
    }

    let repo_path = &args[1];
    let cpv_str = &args[2];
    let phase = &args[3];

    // Parse optional flags
    let mut use_flags: Vec<&str> = Vec::new();
    let mut work_dir: Option<PathBuf> = None;
    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--use" => {
                i += 1;
                while i < args.len() && !args[i].starts_with("--") {
                    use_flags.push(&args[i]);
                    i += 1;
                }
            }
            "--work-dir" => {
                i += 1;
                if i < args.len() {
                    work_dir = Some(PathBuf::from(&args[i]));
                    i += 1;
                }
            }
            other => {
                eprintln!("Unknown option: {other}");
                process::exit(2);
            }
        }
    }

    let repo = match Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error opening repository: {e}");
            process::exit(1);
        }
    };

    let cpv = match Cpv::parse(cpv_str) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Invalid atom {cpv_str}: {e}");
            process::exit(1);
        }
    };

    let category = match repo.category(&cpv.cpn.category) {
        Some(c) => c,
        None => {
            eprintln!("Category {} not found", cpv.cpn.category);
            process::exit(1);
        }
    };

    let package = match category.package(&cpv.cpn.package) {
        Some(p) => p,
        None => {
            eprintln!("Package {}/{} not found", cpv.cpn.category, cpv.cpn.package);
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

    // Determine work root: explicit flag, or $TMPDIR/portage/<cat>/<pf>
    let work_root = work_dir.unwrap_or_else(|| {
        let tmp = env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let pf = format!("{}-{}", cpv.cpn.package, cpv.version);
        PathBuf::from(format!("{tmp}/portage/{}/{pf}", cpv.cpn.category))
    });

    eprintln!(
        ">>> Running phase '{}' for {} in {}",
        phase,
        cpv_str,
        work_root.display()
    );

    let mut shell = match repo.shell().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error creating shell: {e}");
            process::exit(1);
        }
    };

    if !use_flags.is_empty() {
        if let Err(e) = shell.set_use_flags(&use_flags) {
            eprintln!("Error setting USE flags: {e}");
            process::exit(1);
        }
        eprintln!(">>> USE={}", use_flags.join(" "));
    }

    match shell.run_phase(&ebuild, phase, &work_root).await {
        Ok(()) => {
            eprintln!(">>> Phase '{phase}' completed successfully");
        }
        Err(e) => {
            eprintln!("!!! Phase '{phase}' failed: {e}");
            process::exit(1);
        }
    }
}
