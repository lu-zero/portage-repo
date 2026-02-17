//! Rust builtin implementation of `inherit` for eclass sourcing.
//!
//! Reimplements the bash `inherit()` function as a Rust builtin to avoid
//! brush-core's variable scoping bug where arrays defined in a sourced file
//! become invisible after nested `source` calls within bash functions.
//!
//! A builtin's `source_script()` calls happen outside any bash function frame,
//! sidestepping the scoping issue entirely.
//!
//! See [PMS 10.2](https://projects.gentoo.org/pms/9/pms.html#x1-10200010.2)
//! for eclass metadata variable accumulation.

use std::io::Write;
use std::path::PathBuf;

use brush_core::builtins;
use clap::Parser;

/// Accumulating metadata variables (PMS 10.2) for EAPI < 8.
const ACCUM_VARS_BASE: &[&str] = &[
    "IUSE",
    "REQUIRED_USE",
    "DEPEND",
    "BDEPEND",
    "RDEPEND",
    "PDEPEND",
    "IDEPEND",
];

/// Additional accumulating variables for EAPI >= 8.
const ACCUM_VARS_EAPI8: &[&str] = &["PROPERTIES", "RESTRICT"];

/// Source eclasses and manage metadata variable accumulation per PMS 10.
#[derive(Parser)]
pub(crate) struct InheritCommand {
    /// Eclass names to inherit.
    #[arg(required = true)]
    eclasses: Vec<String>,
}

impl builtins::Command for InheritCommand {
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        // Read __INHERIT_DEPTH, default to 0
        let depth: u32 = shell
            .env_str("__INHERIT_DEPTH")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let new_depth = depth + 1;
        set_var(shell, "__INHERIT_DEPTH", &new_depth.to_string());

        // Determine EAPI for conditional accumulation vars
        let eapi: u32 = shell
            .env_str("EAPI")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Build list of accumulating vars
        let accum_vars: Vec<&str> = if eapi >= 8 {
            ACCUM_VARS_BASE
                .iter()
                .chain(ACCUM_VARS_EAPI8.iter())
                .copied()
                .collect()
        } else {
            ACCUM_VARS_BASE.to_vec()
        };

        // Read INHERITED
        let mut inherited = get_var(shell, "INHERITED");

        for eclass in &self.eclasses {
            // Skip if already inherited
            let check = format!(" {eclass} ");
            let padded = format!(" {inherited} ");
            if padded.contains(&check) {
                continue;
            }

            // Find eclass file from __PORTAGE_ECLASS_DIRS
            let eclass_file = find_eclass(shell, eclass);
            let eclass_file = match eclass_file {
                Some(path) => path,
                None => {
                    let _ = writeln!(
                        context.params.stderr(shell),
                        "die: inherit: eclass not found: {eclass}"
                    );
                    return Ok(brush_core::ExecutionResult::new(1));
                }
            };

            // PMS 10.2: save current values and clear for this eclass
            let saved: Vec<(String, String)> = accum_vars
                .iter()
                .map(|&var| {
                    let val = get_var(shell, var);
                    set_var(shell, var, "");
                    (var.to_string(), val)
                })
                .collect();

            // Save/set ECLASS
            let prev_eclass = get_var(shell, "ECLASS");
            set_var(shell, "ECLASS", eclass);

            // Source the eclass file — happens outside any bash function frame
            let params = shell.default_exec_params();
            let result = shell
                .source_script(&eclass_file, std::iter::empty::<&str>(), &params)
                .await;

            if let Err(e) = result {
                let _ = writeln!(
                    context.params.stderr(shell),
                    "die: inherit: failed to source {eclass}: {e}"
                );
                return Ok(brush_core::ExecutionResult::new(1));
            }

            // Restore ECLASS
            set_var(shell, "ECLASS", &prev_eclass);

            // PMS 10.2: read new contributions and combine saved + contribution
            for (var, saved_val) in &saved {
                let contribution = get_var(shell, var);
                let combined = match (saved_val.is_empty(), contribution.is_empty()) {
                    (true, true) => String::new(),
                    (true, false) => contribution,
                    (false, true) => saved_val.clone(),
                    (false, false) => format!("{saved_val} {contribution}"),
                };
                set_var(shell, var, &combined);
            }

            // Append to INHERITED
            if inherited.is_empty() {
                inherited = eclass.clone();
            } else {
                inherited = format!("{inherited} {eclass}");
            }
            set_var(shell, "INHERITED", &inherited);
        }

        // Decrement depth
        let final_depth = new_depth - 1;
        set_var(shell, "__INHERIT_DEPTH", &final_depth.to_string());

        // At depth 0: save accumulated values to __ECLASS_* and clear
        if final_depth == 0 {
            for &var in &accum_vars {
                let val = get_var(shell, var);
                let eclass_key = format!("__ECLASS_{var}");
                set_var(shell, &eclass_key, &val);
                set_var(shell, var, "");
            }
        }

        Ok(brush_core::ExecutionResult::success())
    }
}

/// Read a shell variable, returning empty string if unset.
fn get_var<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>, name: &str) -> String {
    shell
        .env_str(name)
        .map(|cow| cow.into_owned())
        .unwrap_or_default()
}

/// Set a shell variable globally.
fn set_var<SE: brush_core::ShellExtensions>(
    shell: &mut brush_core::Shell<SE>,
    name: &str,
    value: &str,
) {
    let _ = shell.set_env_global(
        name,
        brush_core::ShellVariable::new(brush_core::ShellValue::String(value.to_string())),
    );
}

/// Find an eclass file by searching __PORTAGE_ECLASS_DIRS.
fn find_eclass<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    name: &str,
) -> Option<PathBuf> {
    let dirs = shell.env_str("__PORTAGE_ECLASS_DIRS")?;
    let filename = format!("{name}.eclass");
    for dir in dirs.split(':') {
        let path = PathBuf::from(dir).join(&filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}
