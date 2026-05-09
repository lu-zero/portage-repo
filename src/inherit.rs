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

use camino::Utf8PathBuf;
use std::io::Write;

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

/// All accumulating metadata variables for EAPI >= 8 (base + PROPERTIES + RESTRICT).
const ACCUM_VARS_ALL: &[&str] = &[
    "IUSE",
    "REQUIRED_USE",
    "DEPEND",
    "BDEPEND",
    "RDEPEND",
    "PDEPEND",
    "IDEPEND",
    "PROPERTIES",
    "RESTRICT",
];

/// Precomputed `E_*` variable names parallel to `ACCUM_VARS_BASE`.
pub(crate) const E_VARS_BASE: &[&str] = &[
    "E_IUSE",
    "E_REQUIRED_USE",
    "E_DEPEND",
    "E_BDEPEND",
    "E_RDEPEND",
    "E_PDEPEND",
    "E_IDEPEND",
];

/// Precomputed `E_*` variable names parallel to `ACCUM_VARS_ALL`.
pub(crate) const E_VARS_ALL: &[&str] = &[
    "E_IUSE",
    "E_REQUIRED_USE",
    "E_DEPEND",
    "E_BDEPEND",
    "E_RDEPEND",
    "E_PDEPEND",
    "E_IDEPEND",
    "E_PROPERTIES",
    "E_RESTRICT",
];

/// Source eclasses and manage metadata variable accumulation per PMS 10.
#[derive(Parser)]
pub(crate) struct InheritCommand {
    /// Eclass names to inherit.
    #[arg(required = true)]
    eclasses: Vec<String>,
}

impl builtins::Command for InheritCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        // Determine EAPI for conditional accumulation vars
        let eapi: u32 = shell
            .env_str("EAPI")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let (accum_vars, e_vars): (&[&str], &[&str]) = if eapi >= 8 {
            (ACCUM_VARS_ALL, E_VARS_ALL)
        } else {
            (ACCUM_VARS_BASE, E_VARS_BASE)
        };

        // Direct eclasses are those inherited when ECLASS is not set (top-level
        // ebuild call), as opposed to nested calls from within an eclass.
        let is_top_level = get_var(shell, "ECLASS").is_empty();
        let mut inherit = get_var(shell, "INHERIT");

        // Read INHERITED
        let mut inherited = get_var(shell, "INHERITED");

        for eclass in &self.eclasses {
            // Skip re-sourcing if already inherited transitively, but still
            // record direct inherits in INHERIT for top-level ebuild calls.
            // e.g. `inherit acct-group user-info` where acct-group.eclass
            // already pulled in user-info: user-info is skipped for sourcing
            // but must still appear in INHERIT.
            if inherited.split_whitespace().any(|e| e == eclass) {
                if is_top_level {
                    if !inherit.is_empty() {
                        inherit.push(' ');
                    }
                    inherit.push_str(eclass);
                    set_var(shell, "INHERIT", &inherit);
                }
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

            // PMS 10.2: save current accum var values (B_* pattern) and clear them.
            // Each eclass sees empty vars, so its assignments are its own contribution.
            // Prior accumulated values are restored afterwards; they are not visible
            // to the eclass being sourced.
            let saved: Vec<(&'static str, String)> = accum_vars
                .iter()
                .map(|&var| {
                    let val = get_var(shell, var);
                    set_var(shell, var, "");
                    (var, val)
                })
                .collect();

            // Save/set ECLASS
            let prev_eclass = get_var(shell, "ECLASS");
            set_var(shell, "ECLASS", eclass);

            // Source the eclass file — happens outside any bash function frame
            let params = shell.default_exec_params();
            let result = shell
                .source_script(
                    eclass_file.as_std_path(),
                    std::iter::empty::<&str>(),
                    &params,
                )
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

            // PMS 10.2: append eclass contribution to E_{VAR} and restore B_*.
            //
            // Mirrors Portage's ebuild.sh pattern:
            //   [[ -v VAR ]] && E_VAR+=" ${VAR}"
            //   [[ -v B_VAR ]] && VAR="${B_VAR}" || unset VAR
            //
            // This preserves each eclass's independent contribution even when an
            // eclass unconditionally assigns (rather than appends to) a variable.
            for ((var, saved_val), &e_var) in saved.iter().zip(e_vars.iter()) {
                let contribution = get_var(shell, var);
                let e_val = get_var(shell, e_var);
                let new_e_val = match (e_val.is_empty(), contribution.is_empty()) {
                    (_, true) => e_val,
                    (true, false) => contribution,
                    (false, false) => format!("{e_val} {contribution}"),
                };
                set_var(shell, e_var, &new_e_val);
                // Restore saved (B_*) value
                set_var(shell, var, saved_val);
            }

            // Re-read INHERITED: sourcing the eclass may have updated it via
            // nested `inherit` calls (e.g. acct-group.eclass inherits user-info).
            inherited = get_var(shell, "INHERITED");

            // Append to INHERITED (transitive list — all recursively inherited eclasses)
            if !inherited.is_empty() {
                inherited.push(' ');
            }
            inherited.push_str(eclass);
            set_var(shell, "INHERITED", &inherited);

            // Append to INHERIT (direct list — only eclasses from the ebuild itself)
            if is_top_level {
                if !inherit.is_empty() {
                    inherit.push(' ');
                }
                inherit.push_str(eclass);
                set_var(shell, "INHERIT", &inherit);
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
) -> Option<Utf8PathBuf> {
    let dirs = shell.env_str("__PORTAGE_ECLASS_DIRS")?;
    let filename = format!("{name}.eclass");
    for dir in dirs.split(':') {
        let path = Utf8PathBuf::from(dir).join(&filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}
