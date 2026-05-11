//! Rust builtins for PMS utility functions: eapi predicates, phase setup,
//! output helpers, build helpers, and PMS 12.3 USE/has/ver functions.
//!
//! These reimplement the functionality of portage's `eapi.sh`,
//! `isolated-functions.sh`, `phase-helpers.sh`, and `phase-functions.sh`
//! without sourcing those files.  Rust builtins sidestep brush parser gaps
//! (notably bare `[[ ]]` function bodies used as EAPI predicate bodies).
//!
//! See [PMS 12.3](https://projects.gentoo.org/pms/9/pms.html#available-commands).

use std::io::Write;

use brush_core::builtins;
use clap::Parser;

// ── die ───────────────────────────────────────────────────────────────────────

/// `die [message]`  (PMS 12.2.1)
///
/// Prints `die: <message>` to stderr and returns 1.
/// The "die: " prefix is load-bearing — it is matched by tests and by
/// `inherit` error paths to distinguish portage die output from other stderr.
#[derive(Parser)]
pub(crate) struct DieCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for DieCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let msg = self.message.join(" ");
        let _ = writeln!(context.params.stderr(shell), "die: {msg}");
        Ok(brush_core::ExecutionResult::new(1))
    }
}

// ── EXPORT_FUNCTIONS ──────────────────────────────────────────────────────────

/// `EXPORT_FUNCTIONS <phase> [phase...]`  (PMS 10.2)
///
/// For each named phase, defines a wrapper function in the shell:
///   `${phase}() { ${ECLASS}_${phase} "$@"; }`
///
/// Ported from bash `eval` to a batched `run_string` call, eliminating the
/// bash for-loop + per-phase eval overhead.
#[derive(Parser)]
pub(crate) struct ExportFunctionsCommand {
    #[arg(required = true)]
    phases: Vec<String>,
}

impl builtins::Command for ExportFunctionsCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        let eclass = match shell.env_str("ECLASS") {
            Some(e) if !e.is_empty() => e.into_owned(),
            _ => {
                let _ = writeln!(
                    context.params.stderr(shell),
                    "die: EXPORT_FUNCTIONS called outside eclass scope"
                );
                return Ok(brush_core::ExecutionResult::new(1));
            }
        };

        // Build all wrapper definitions as a single script and parse once.
        let script: String = self
            .phases
            .iter()
            .map(|phase| format!("{phase}() {{ {eclass}_{phase} \"$@\"; }}\n"))
            .collect();

        let source_info = brush_core::SourceInfo::from("EXPORT_FUNCTIONS");
        let params = shell.default_exec_params();
        shell.run_string(&script, &source_info, &params).await?;

        Ok(brush_core::ExecutionResult::success())
    }
}

// ── has / hasv / hasq ─────────────────────────────────────────────────────────

/// `has <needle> [haystack...]`  (PMS 12.3.4)
///
/// Returns 0 (success) if needle equals any haystack word, 1 otherwise.
/// `hasq` is a deprecated alias registered under a separate name.
#[derive(Parser)]
pub(crate) struct HasCommand {
    needle: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    haystack: Vec<String>,
}

impl builtins::Command for HasCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let found = self.haystack.iter().any(|item| item == &self.needle);
        Ok(brush_core::ExecutionResult::new(u8::from(!found)))
    }
}

/// `hasv <needle> [haystack...]`  (PMS 12.3.4)
///
/// Like `has`, but also prints the needle to stdout if found.
#[derive(Parser)]
pub(crate) struct HasvCommand {
    needle: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    haystack: Vec<String>,
}

impl builtins::Command for HasvCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        if self.haystack.iter().any(|item| item == &self.needle) {
            let _ = writeln!(context.params.stdout(shell), "{}", self.needle);
            Ok(brush_core::ExecutionResult::success())
        } else {
            Ok(brush_core::ExecutionResult::new(1))
        }
    }
}

// ── use ───────────────────────────────────────────────────────────────────────

/// `use <flag>`  (PMS 12.3.1)
///
/// Returns 0 if flag is present as a whole word in `$USE`, 1 otherwise.
#[derive(Parser)]
pub(crate) struct UseCommand {
    #[arg(allow_hyphen_values = true)]
    flag: String,
}

impl builtins::Command for UseCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let enabled = use_flag_enabled(context.shell, &self.flag);
        Ok(brush_core::ExecutionResult::new(u8::from(!enabled)))
    }
}

// ── usev ──────────────────────────────────────────────────────────────────────

/// `usev <flag> [true-val]`  (PMS 12.3.6)
///
/// If flag is set: prints flag (or true-val if given) and returns 0.
/// If flag is unset: prints nothing and returns 1.
#[derive(Parser)]
pub(crate) struct UsevCommand {
    #[arg(allow_hyphen_values = true)]
    flag: String,
    true_val: Option<String>,
}

impl builtins::Command for UsevCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        if use_flag_enabled(shell, &self.flag) {
            let out = self.true_val.as_deref().unwrap_or(&self.flag);
            let _ = writeln!(context.params.stdout(shell), "{out}");
            Ok(brush_core::ExecutionResult::success())
        } else {
            Ok(brush_core::ExecutionResult::new(1))
        }
    }
}

// ── usex ──────────────────────────────────────────────────────────────────────

/// `usex <flag> [true-str [false-str [true-suffix [false-suffix]]]]`  (PMS 12.3.7)
///
/// Prints `${true-str}${true-suffix}` (defaults: "yes", "") if flag is set,
/// or `${false-str}${false-suffix}` (defaults: "no", "") if not.
/// Returns 0 if flag is set, 1 otherwise.
#[derive(Parser)]
pub(crate) struct UsexCommand {
    #[arg(allow_hyphen_values = true)]
    flag: String,
    true_str: Option<String>,
    false_str: Option<String>,
    true_suffix: Option<String>,
    false_suffix: Option<String>,
}

impl builtins::Command for UsexCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        if use_flag_enabled(shell, &self.flag) {
            let s = self.true_str.as_deref().unwrap_or("yes");
            let sfx = self.true_suffix.as_deref().unwrap_or("");
            let _ = writeln!(context.params.stdout(shell), "{s}{sfx}");
            Ok(brush_core::ExecutionResult::success())
        } else {
            let s = self.false_str.as_deref().unwrap_or("no");
            let sfx = self.false_suffix.as_deref().unwrap_or("");
            let _ = writeln!(context.params.stdout(shell), "{s}{sfx}");
            Ok(brush_core::ExecutionResult::new(1))
        }
    }
}

// ── use_enable / use_with ─────────────────────────────────────────────────────

/// `use_enable <flag> [feature [value]]`  (PMS 12.3.8)
///
/// Outputs `--enable-feature[=value]` or `--disable-feature`.
#[derive(Parser)]
pub(crate) struct UseEnableCommand {
    #[arg(allow_hyphen_values = true)]
    flag: String,
    feature: Option<String>,
    val: Option<String>,
}

impl builtins::Command for UseEnableCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let feature = self.feature.as_deref().unwrap_or(&self.flag);
        let out = if use_flag_enabled(shell, &self.flag) {
            match &self.val {
                Some(v) => format!("--enable-{feature}={v}"),
                None => format!("--enable-{feature}"),
            }
        } else {
            format!("--disable-{feature}")
        };
        let _ = writeln!(context.params.stdout(shell), "{out}");
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `use_with <flag> [feature [value]]`  (PMS 12.3.9)
///
/// Outputs `--with-feature[=value]` or `--without-feature`.
#[derive(Parser)]
pub(crate) struct UseWithCommand {
    #[arg(allow_hyphen_values = true)]
    flag: String,
    feature: Option<String>,
    val: Option<String>,
}

impl builtins::Command for UseWithCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let feature = self.feature.as_deref().unwrap_or(&self.flag);
        let out = if use_flag_enabled(shell, &self.flag) {
            match &self.val {
                Some(v) => format!("--with-{feature}={v}"),
                None => format!("--with-{feature}"),
            }
        } else {
            format!("--without-{feature}")
        };
        let _ = writeln!(context.params.stdout(shell), "{out}");
        Ok(brush_core::ExecutionResult::success())
    }
}

// ── in_iuse ───────────────────────────────────────────────────────────────────

/// `in_iuse <flag>`  (PMS 12.3.5)
///
/// Returns 0 if flag appears in `$IUSE` (stripping any leading +/- prefix).
#[derive(Parser)]
pub(crate) struct InIuseCommand {
    #[arg(allow_hyphen_values = true)]
    flag: String,
}

impl builtins::Command for InIuseCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let iuse = shell
            .env_str("IUSE")
            .map(|c| c.into_owned())
            .unwrap_or_default();
        let found = iuse
            .split_whitespace()
            .any(|entry| entry.trim_start_matches(['+', '-']) == self.flag);
        Ok(brush_core::ExecutionResult::new(u8::from(!found)))
    }
}

// ── ver_replacing ─────────────────────────────────────────────────────────────

/// `ver_replacing`  (PMS 12.3.14 / EAPI 9)
///
/// Outputs the versions being replaced, one per line.  During metadata
/// extraction no package is being replaced, so the output is always empty.
///
/// See [PMS 12.3.14](https://projects.gentoo.org/pms/9/pms.html#ver-funcs).
#[derive(Parser)]
pub(crate) struct VerReplacingCommand {}

impl builtins::Command for VerReplacingCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        _context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        // No package is being replaced during metadata extraction.
        Ok(brush_core::ExecutionResult::new(0))
    }
}

// ── ___eapi_* predicates ──────────────────────────────────────────────────────

/// All 74 `___eapi_*` EAPI predicate functions from portage's `eapi.sh`.
///
/// Each checks whether a given EAPI has a specific feature.  Takes an
/// optional first argument to override `$EAPI`.
///
/// Registered under every `___eapi_*` name; dispatches via `command_name`.
#[derive(Parser)]
pub(crate) struct EapiPredicateCommand {
    eapi_override: Option<String>,
}

impl builtins::Command for EapiPredicateCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let eapi: u32 = if let Some(s) = &self.eapi_override {
            s.parse().unwrap_or(0)
        } else {
            context
                .shell
                .env_str("EAPI")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        };
        let result = eapi_predicate(&context.command_name, eapi);
        Ok(brush_core::ExecutionResult::new(u8::from(!result)))
    }
}

fn eapi_predicate(name: &str, eapi: u32) -> bool {
    match name {
        "___eapi_default_src_test_disables_parallel_jobs" => eapi <= 4,
        "___eapi_has_S_WORKDIR_fallback" => eapi <= 3,
        "___eapi_has_pkg_pretend" => eapi >= 4,
        "___eapi_has_src_configure" => eapi >= 2,
        "___eapi_has_src_prepare" => eapi >= 2,
        "___eapi_has_BDEPEND" => eapi >= 7,
        "___eapi_has_BROOT" => eapi >= 7,
        "___eapi_has_IDEPEND" => eapi >= 8,
        "___eapi_has_PORTDIR_ECLASSDIR" => eapi <= 6,
        "___eapi_has_RDEPEND_DEPEND_fallback" => eapi <= 3,
        "___eapi_has_SYSROOT" => eapi >= 7,
        "___eapi_has_accumulated_PROPERTIES" => eapi >= 8,
        "___eapi_has_accumulated_RESTRICT" => eapi >= 8,
        "___eapi_has_prefix_variables" => eapi >= 3,
        "___eapi_has_assert" => eapi <= 8,
        "___eapi_has_docompress" => eapi >= 4,
        "___eapi_has_dohard" => eapi <= 3,
        "___eapi_has_doheader" => eapi >= 5,
        "___eapi_has_dohtml" => eapi <= 6,
        "___eapi_has_dolib_libopts" => eapi <= 6,
        "___eapi_has_domo" => eapi <= 8,
        "___eapi_has_dosed" => eapi <= 3,
        "___eapi_has_dostrip" => eapi >= 7,
        "___eapi_has_eapply" => eapi >= 6,
        "___eapi_has_eapply_user" => eapi >= 6,
        "___eapi_has_edo" => eapi >= 9,
        "___eapi_has_einstall" => eapi <= 5,
        "___eapi_has_einstalldocs" => eapi >= 6,
        "___eapi_has_get_libdir" => eapi >= 6,
        "___eapi_has_hasq" => eapi <= 7,
        "___eapi_has_hasv" => eapi <= 7,
        "___eapi_has_in_iuse" => eapi >= 6,
        "___eapi_has_nonfatal" => eapi >= 4,
        "___eapi_has_pipestatus" => eapi >= 9,
        "___eapi_has_useq" => eapi <= 7,
        "___eapi_has_usex" => eapi >= 5,
        "___eapi_has_ver_replacing" => eapi >= 9,
        "___eapi_has_version_functions" => eapi >= 7,
        "___eapi_best_version_and_has_version_support_--host-root" => {
            eapi == 5 || eapi == 6
        }
        "___eapi_best_version_and_has_version_support_-b_-d_-r" => eapi >= 7,
        "___eapi_die_can_respect_nonfatal" => eapi >= 6,
        "___eapi_doconfd_respects_insopts" => eapi <= 7,
        "___eapi_dodoc_supports_-r" => eapi >= 4,
        "___eapi_doenvd_respects_insopts" => eapi <= 7,
        "___eapi_doheader_respects_insopts" => eapi <= 7,
        "___eapi_doinitd_respects_exeopts" => eapi <= 7,
        "___eapi_doins_and_newins_preserve_symlinks" => eapi >= 4,
        "___eapi_domo_respects_into" => eapi <= 6,
        "___eapi_econf_passes_--datarootdir" => eapi >= 8,
        "___eapi_econf_passes_--disable-dependency-tracking" => eapi >= 4,
        "___eapi_econf_passes_--disable-silent-rules" => eapi >= 5,
        "___eapi_econf_passes_--disable-static" => eapi >= 8,
        "___eapi_econf_passes_--docdir_and_--htmldir" => eapi >= 6,
        "___eapi_econf_passes_--with-sysroot" => eapi >= 7,
        "___eapi_has_DESTTREE_INSDESTTREE" => eapi <= 6,
        "___eapi_has_dosym_r" => eapi >= 8,
        "___eapi_helpers_can_die" => eapi >= 4,
        "___eapi_newins_supports_reading_from_standard_input" => eapi >= 5,
        "___eapi_unpack_is_case_sensitive" => eapi <= 5,
        "___eapi_unpack_supports_7z" => eapi <= 7,
        "___eapi_unpack_supports_absolute_paths" => eapi >= 6,
        "___eapi_unpack_supports_lha" => eapi <= 7,
        "___eapi_unpack_supports_rar" => eapi <= 7,
        "___eapi_unpack_supports_txz" => eapi >= 6,
        "___eapi_unpack_supports_xz" => eapi >= 3,
        "___eapi_use_enable_and_use_with_support_empty_third_argument" => eapi >= 4,
        "___eapi_usev_has_second_arg" => eapi >= 8,
        "___eapi_bash_3_2" => eapi <= 5,
        "___eapi_bash_4_2" => eapi == 6 || eapi == 7,
        "___eapi_bash_5_0" => eapi == 8,
        "___eapi_bash_5_3" => eapi >= 9,
        "___eapi_enables_failglob_in_global_scope" => eapi >= 6,
        "___eapi_has_ENV_UNSET" => eapi >= 7,
        "___eapi_has_strict_keepdir" => eapi >= 8,
        _ => false,
    }
}

/// All `___eapi_*` predicate names; used during builtin registration.
pub(crate) const EAPI_PREDICATE_NAMES: &[&str] = &[
    "___eapi_default_src_test_disables_parallel_jobs",
    "___eapi_has_S_WORKDIR_fallback",
    "___eapi_has_pkg_pretend",
    "___eapi_has_src_configure",
    "___eapi_has_src_prepare",
    "___eapi_has_BDEPEND",
    "___eapi_has_BROOT",
    "___eapi_has_IDEPEND",
    "___eapi_has_PORTDIR_ECLASSDIR",
    "___eapi_has_RDEPEND_DEPEND_fallback",
    "___eapi_has_SYSROOT",
    "___eapi_has_accumulated_PROPERTIES",
    "___eapi_has_accumulated_RESTRICT",
    "___eapi_has_prefix_variables",
    "___eapi_has_assert",
    "___eapi_has_docompress",
    "___eapi_has_dohard",
    "___eapi_has_doheader",
    "___eapi_has_dohtml",
    "___eapi_has_dolib_libopts",
    "___eapi_has_domo",
    "___eapi_has_dosed",
    "___eapi_has_dostrip",
    "___eapi_has_eapply",
    "___eapi_has_eapply_user",
    "___eapi_has_edo",
    "___eapi_has_einstall",
    "___eapi_has_einstalldocs",
    "___eapi_has_get_libdir",
    "___eapi_has_hasq",
    "___eapi_has_hasv",
    "___eapi_has_in_iuse",
    "___eapi_has_nonfatal",
    "___eapi_has_pipestatus",
    "___eapi_has_useq",
    "___eapi_has_usex",
    "___eapi_has_ver_replacing",
    "___eapi_has_version_functions",
    "___eapi_best_version_and_has_version_support_--host-root",
    "___eapi_best_version_and_has_version_support_-b_-d_-r",
    "___eapi_die_can_respect_nonfatal",
    "___eapi_doconfd_respects_insopts",
    "___eapi_dodoc_supports_-r",
    "___eapi_doenvd_respects_insopts",
    "___eapi_doheader_respects_insopts",
    "___eapi_doinitd_respects_exeopts",
    "___eapi_doins_and_newins_preserve_symlinks",
    "___eapi_domo_respects_into",
    "___eapi_econf_passes_--datarootdir",
    "___eapi_econf_passes_--disable-dependency-tracking",
    "___eapi_econf_passes_--disable-silent-rules",
    "___eapi_econf_passes_--disable-static",
    "___eapi_econf_passes_--docdir_and_--htmldir",
    "___eapi_econf_passes_--with-sysroot",
    "___eapi_has_DESTTREE_INSDESTTREE",
    "___eapi_has_dosym_r",
    "___eapi_helpers_can_die",
    "___eapi_newins_supports_reading_from_standard_input",
    "___eapi_unpack_is_case_sensitive",
    "___eapi_unpack_supports_7z",
    "___eapi_unpack_supports_absolute_paths",
    "___eapi_unpack_supports_lha",
    "___eapi_unpack_supports_rar",
    "___eapi_unpack_supports_txz",
    "___eapi_unpack_supports_xz",
    "___eapi_use_enable_and_use_with_support_empty_third_argument",
    "___eapi_usev_has_second_arg",
    "___eapi_bash_3_2",
    "___eapi_bash_4_2",
    "___eapi_bash_5_0",
    "___eapi_bash_5_3",
    "___eapi_enables_failglob_in_global_scope",
    "___eapi_has_ENV_UNSET",
    "___eapi_has_strict_keepdir",
];

// ── __ebuild_phase_funcs ──────────────────────────────────────────────────────

/// `__ebuild_phase_funcs <eapi> <phase_func>`  (portage `phase-functions.sh`)
///
/// Sets up `default()` and `default_<phase_func>()` pointing at the correct
/// EAPI-versioned implementation, and installs a fallback `<phase_func>()`
/// that calls `default` if the ebuild did not define the phase itself.
#[derive(Parser)]
pub(crate) struct EbuildPhaseFuncsCommand {
    eapi_str: String,
    phase_func: String,
}

impl builtins::Command for EbuildPhaseFuncsCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let eapi: u32 = self.eapi_str.parse().unwrap_or(0);
        let phase = &self.phase_func;

        let mut script = String::new();

        if eapi <= 1 {
            // EAPI 0/1 has no 'default' mechanism; define missing phase funcs directly.
            for name in &["pkg_nofetch", "src_unpack", "src_test"] {
                if *name == phase {
                    script += &format!("declare -F {name} >/dev/null || {name}() {{ __eapi0_{name}; }}\n");
                }
            }
            if phase == "src_compile" {
                let impl_fn = if eapi == 0 {
                    "__eapi0_src_compile"
                } else {
                    "__eapi1_src_compile"
                };
                script += &format!(
                    "declare -F src_compile >/dev/null || src_compile() {{ {impl_fn}; }}\n"
                );
            }
        } else {
            // EAPI 2+: define default() and default_<phase>().
            script += &format!("default() {{ default_{phase}; }}\n");

            if let Some(impl_fn) = resolve_phase_default(eapi, phase) {
                script += &format!("default_{phase}() {{ {impl_fn}; }}\n");
            } else {
                script += &format!(
                    "default_{phase}() {{ die \"default_{phase} has no implementation in EAPI {eapi}\"; }}\n"
                );
            }

            // Install fallback only when the ebuild did not define the phase.
            let phase_defined = shell.funcs().get(phase.as_str()).is_some();
            if !phase_defined {
                script += &format!("{phase}() {{ default; }}\n");
            }
        }

        let source_info = brush_core::SourceInfo::from("__ebuild_phase_funcs");
        let params = shell.default_exec_params();
        shell.run_string(&script, &source_info, &params).await?;

        Ok(brush_core::ExecutionResult::success())
    }
}

/// Return the name of the bash function that implements the default behaviour
/// for `phase_func` in the given EAPI.
fn resolve_phase_default(eapi: u32, phase_func: &str) -> Option<&'static str> {
    match phase_func {
        "pkg_nofetch" => Some("__eapi0_pkg_nofetch"),
        "src_unpack" => Some("__eapi0_src_unpack"),
        "src_test" => Some("__eapi0_src_test"),
        "src_configure" => Some("__eapi2_src_configure"),
        "src_compile" => Some("__eapi2_src_compile"),
        "src_prepare" => {
            if eapi >= 8 {
                Some("__eapi8_src_prepare")
            } else if eapi >= 6 {
                Some("__eapi6_src_prepare")
            } else {
                Some("__eapi2_src_prepare")
            }
        }
        "src_install" => {
            if eapi >= 6 {
                Some("__eapi6_src_install")
            } else if eapi >= 4 {
                Some("__eapi4_src_install")
            } else {
                None
            }
        }
        _ => None,
    }
}

// ── P1 output helpers ─────────────────────────────────────────────────────────

/// `einfo/elog/ewarn/eerror/eqawarn/einfon <message>`
///
/// Prints ` * <message>` to stderr.  All these commands share the same format
/// in a plain terminal; colour is portage's concern.
#[derive(Parser)]
pub(crate) struct EchoMessageCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EchoMessageCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let msg = self.message.join(" ");
        let _ = writeln!(context.params.stderr(shell), " * {msg}");
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `ebegin <message>`
///
/// Prints ` * <message> ...` to stderr (beginning of a timed action).
#[derive(Parser)]
pub(crate) struct EbeginCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EbeginCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let msg = self.message.join(" ");
        let _ = writeln!(context.params.stderr(shell), " * {msg} ...");
        Ok(brush_core::ExecutionResult::success())
    }
}

/// `eend [exit_code] [message]`
///
/// Prints `[ ok ]` (exit_code 0) or `[ !! ] message` (exit_code non-zero).
#[derive(Parser)]
pub(crate) struct EendCommand {
    exit_code: Option<u8>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for EendCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let code = self.exit_code.unwrap_or(0);
        if code == 0 {
            let _ = writeln!(context.params.stderr(shell), " [ ok ]");
        } else {
            let msg = self.message.join(" ");
            let _ = writeln!(context.params.stderr(shell), " [ !! ] {msg}");
        }
        Ok(brush_core::ExecutionResult::new(code))
    }
}

// ── P2 build helpers ──────────────────────────────────────────────────────────

/// `emake [args...]`  (PMS 12.3.2)
///
/// Runs `${MAKE:-make} ${MAKEOPTS} ${EXTRA_EMAKE} [args...]`.
#[derive(Parser)]
pub(crate) struct EmakeCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for EmakeCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let make = shell
            .env_str("MAKE")
            .map(|s| s.into_owned())
            .unwrap_or_else(|| "make".to_string());
        let makeopts_str = shell
            .env_str("MAKEOPTS")
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let extra_str = shell
            .env_str("EXTRA_EMAKE")
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let makeopts: Vec<String> = makeopts_str
            .split_whitespace()
            .map(|s| s.to_owned())
            .collect();
        let extra: Vec<String> = extra_str
            .split_whitespace()
            .map(|s| s.to_owned())
            .collect();
        let args = self.args.clone();
        let cwd = shell.working_dir().to_path_buf();

        let exit = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&make)
                .current_dir(&cwd)
                .args(&makeopts)
                .args(&extra)
                .args(&args)
                .status()
                .map(|s| s.code().unwrap_or(1) as u8)
                .unwrap_or(127)
        })
        .await
        .unwrap_or(127);

        Ok(brush_core::ExecutionResult::new(exit))
    }
}

/// `econf [extra-args...]`
///
/// Runs `./configure` (or `$ECONF_SOURCE/configure`) with standard flags
/// derived from the current EAPI and portage environment variables.
#[derive(Parser)]
pub(crate) struct EconfCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for EconfCommand {
    type State = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        let get = |var: &str| shell.env_str(var).map(|s| s.into_owned()).unwrap_or_default();
        let eapi: u32 = get("EAPI").parse().unwrap_or(0);
        let econf_source = {
            let s = get("ECONF_SOURCE");
            if s.is_empty() { ".".to_string() } else { s }
        };
        let eprefix    = get("EPREFIX");
        let pf         = get("PF");
        let chost      = get("CHOST");
        let cbuild     = get("CBUILD");
        let ctarget    = get("CTARGET");
        let esysroot   = { let s = get("ESYSROOT"); if s.is_empty() { "/".to_string() } else { s } };
        let extra_econf = get("EXTRA_ECONF");

        let mut env_vars: Vec<(String, String)> = Vec::new();
        for var in &[
            "CC", "CXX", "AR", "RANLIB", "NM", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS",
            "CONFIG_SHELL",
        ] {
            if let Some(val) = shell.env_str(var) {
                env_vars.push((var.to_string(), val.into_owned()));
            }
        }

        let user_args = self.args.clone();
        let cwd = shell.working_dir().to_path_buf();

        let exit = tokio::task::spawn_blocking(move || {
            // configure path: $ECONF_SOURCE/configure, defaulting to $S (cwd).
            let base = if econf_source == "." {
                cwd.clone()
            } else {
                std::path::PathBuf::from(&econf_source)
            };
            let configure = base.join("configure");
            if !configure.exists() {
                return 0u8;
            }

            // Probe EAPI-conditional flags from configure --help.
            let help = if eapi >= 4 {
                std::process::Command::new(&configure)
                    .arg("--help")
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let mut conf_args: Vec<String> = Vec::new();

            conf_args.push(format!("--prefix={eprefix}/usr"));
            if !cbuild.is_empty() { conf_args.push(format!("--build={cbuild}")); }
            if !chost.is_empty()  { conf_args.push(format!("--host={chost}")); }
            if !ctarget.is_empty() { conf_args.push(format!("--target={ctarget}")); }
            conf_args.push(format!("--mandir={eprefix}/usr/share/man"));
            conf_args.push(format!("--infodir={eprefix}/usr/share/info"));
            conf_args.push(format!("--datadir={eprefix}/usr/share"));
            conf_args.push(format!("--sysconfdir={eprefix}/etc"));
            conf_args.push(format!("--localstatedir={eprefix}/var/lib"));

            if eapi >= 8 && help.contains("--datarootdir") {
                conf_args.push(format!("--datarootdir={eprefix}/usr/share"));
            }
            // Use word-boundary guard matching portage's pattern.
            if eapi >= 4 && contains_flag(&help, "--disable-dependency-tracking") {
                conf_args.push("--disable-dependency-tracking".to_string());
            }
            if eapi >= 5 && contains_flag(&help, "--disable-silent-rules") {
                conf_args.push("--disable-silent-rules".to_string());
            }
            if eapi >= 6 {
                if help.contains("--docdir") {
                    conf_args.push(format!("--docdir={eprefix}/usr/share/doc/{pf}"));
                }
                if help.contains("--htmldir") {
                    conf_args.push(format!("--htmldir={eprefix}/usr/share/doc/{pf}/html"));
                }
            }
            if eapi >= 7 && contains_flag(&help, "--with-sysroot") {
                conf_args.push(format!("--with-sysroot={esysroot}"));
            }
            // Portage requires both --enable-shared and --enable-static before adding
            // --disable-static, to avoid touching packages that don't support static builds.
            if eapi >= 8
                && contains_flag(&help, "--enable-shared")
                && contains_flag(&help, "--enable-static")
            {
                conf_args.push("--disable-static".to_string());
            }

            conf_args.extend(user_args);
            // EXTRA_ECONF is split on whitespace; quoted-whitespace in values is rare
            // in practice (portage eval's it, which we can't do safely here).
            conf_args.extend(extra_econf.split_whitespace().map(str::to_owned));

            let mut cmd = std::process::Command::new(&configure);
            cmd.current_dir(&cwd).args(&conf_args);
            for (k, v) in &env_vars {
                cmd.env(k, v);
            }

            cmd.status()
                .map(|s| s.code().unwrap_or(1) as u8)
                .unwrap_or(127)
        })
        .await
        .unwrap_or(127);

        Ok(brush_core::ExecutionResult::new(exit))
    }
}

// ── shared helpers ────────────────────────────────────────────────────────────

/// Returns true if `flag` appears in `text` followed by a non-identifier character
/// (space, newline, `=`, end-of-string), matching portage's word-boundary guard.
/// Prevents `--disable-dependency-tracking` from matching `--disable-dependency-tracking-fast`.
fn contains_flag(text: &str, flag: &str) -> bool {
    let mut rest = text;
    while let Some(pos) = rest.find(flag) {
        let after = &rest[pos + flag.len()..];
        if after
            .chars()
            .next()
            .map_or(true, |c| !c.is_ascii_alphanumeric() && !"+_.-".contains(c))
        {
            return true;
        }
        rest = &rest[pos + 1..];
    }
    false
}

/// Returns true if `flag` appears as a whole word in the shell's `$USE`.
fn use_flag_enabled<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    flag: &str,
) -> bool {
    shell
        .env_str("USE")
        .map(|use_val| use_val.split_whitespace().any(|f| f == flag))
        .unwrap_or(false)
}
