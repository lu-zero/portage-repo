//! Rust builtins for PMS 12.3 utility functions: `has`, `use`, and friends.
//!
//! These replace the equivalent bash function bodies in [`crate::builtins`].
//! Implementing them as Rust builtins avoids bash interpretation overhead for
//! functions that are called on every ebuild and eclass sourced.
//!
//! See [PMS 12.3](https://projects.gentoo.org/pms/9/pms.html#available-commands).

use std::io::Write;

use brush_core::builtins;
use clap::Parser;

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
        let found = iuse.split_whitespace().any(|entry| {
            entry.trim_start_matches(['+', '-']) == self.flag
        });
        Ok(brush_core::ExecutionResult::new(u8::from(!found)))
    }
}

// ── shared helper ─────────────────────────────────────────────────────────────

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
