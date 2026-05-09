use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camino::Utf8PathBuf;

use brush_builtins::ShellBuilderExt;
use brush_core::parser::ParserImpl;
use brush_core::{
    ProfileLoadBehavior, RcLoadBehavior, Shell, ShellValue, ShellVariable, SourceInfo,
};
use portage_metadata::{Eapi, EbuildMetadata, Phase};

use crate::builtins;
use crate::ebuild::Ebuild;
use crate::error::{Error, Result};
use crate::inherit;
use crate::pms_builtins;
use crate::repository::Repository;
use crate::ver_funcs;

/// Metadata variables extracted from a sourced ebuild.
///
/// These correspond to the PMS-defined metadata variables that an ebuild
/// is expected to define after being sourced.
const METADATA_VARS: &[&str] = &[
    "DESCRIPTION",
    "HOMEPAGE",
    "SRC_URI",
    "LICENSE",
    "SLOT",
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
    "INHERIT",
    "INHERITED",
];

/// Maps a portage phase name to (EBUILD_PHASE value, function name).
fn phase_to_func(phase: &str) -> (&str, &str) {
    match phase {
        "pretend"   => ("pretend",   "pkg_pretend"),
        "setup"     => ("setup",     "pkg_setup"),
        "unpack"    => ("unpack",    "src_unpack"),
        "prepare"   => ("prepare",   "src_prepare"),
        "configure" => ("configure", "src_configure"),
        "compile"   => ("compile",   "src_compile"),
        "test"      => ("test",      "src_test"),
        "install"   => ("install",   "src_install"),
        "preinst"   => ("preinst",   "pkg_preinst"),
        "postinst"  => ("postinst",  "pkg_postinst"),
        "prerm"     => ("prerm",     "pkg_prerm"),
        "postrm"    => ("postrm",    "pkg_postrm"),
        "nofetch"   => ("nofetch",   "pkg_nofetch"),
        "info"      => ("info",      "pkg_info"),
        "config"    => ("config",    "pkg_config"),
        // accept raw function names too
        other       => (other, other),
    }
}

/// PMS phase function names mapped to their [`Phase`] variants.
///
/// Used to compute `DEFINED_PHASES` by inspecting which functions are
/// defined in the shell after sourcing an ebuild.
///
/// See [PMS 7.4](https://projects.gentoo.org/pms/9/pms.html#defined-phases).
const PHASE_FUNCTIONS: &[(&str, Phase)] = &[
    ("pkg_pretend", Phase::PkgPretend),
    ("pkg_setup", Phase::PkgSetup),
    ("src_unpack", Phase::SrcUnpack),
    ("src_prepare", Phase::SrcPrepare),
    ("src_configure", Phase::SrcConfigure),
    ("src_compile", Phase::SrcCompile),
    ("src_test", Phase::SrcTest),
    ("src_install", Phase::SrcInstall),
    ("pkg_preinst", Phase::PkgPreinst),
    ("pkg_postinst", Phase::PkgPostinst),
    ("pkg_prerm", Phase::PkgPrerm),
    ("pkg_postrm", Phase::PkgPostrm),
    ("pkg_config", Phase::PkgConfig),
    ("pkg_info", Phase::PkgInfo),
    ("pkg_nofetch", Phase::PkgNofetch),
];

/// An embedded bash shell for sourcing ebuilds, eclasses, and `make.defaults`.
///
/// Wraps [`brush_core::Shell`] configured for Gentoo ebuild evaluation.
/// The shell has standard bash builtins registered and eclass directories
/// set up for the repository.
///
/// See [PMS 7](https://projects.gentoo.org/pms/9/pms.html#ebuilddefined-variables)
/// for the metadata variables extracted after sourcing an ebuild.
pub struct EbuildShell {
    shell: Shell,
    repo_path: Utf8PathBuf,
    eclass_dirs: Vec<Utf8PathBuf>,
    /// Active USE flags for this shell session.
    /// Used by the `use()`, `usev()`, `usex()` functions.
    use_flags: HashSet<String>,
}

impl EbuildShell {
    /// Create a new shell configured for the given repository.
    ///
    /// Registers Portage-specific bash functions (`inherit`, `die`,
    /// `EXPORT_FUNCTIONS`, etc.) and sets up eclass directories from
    /// the repository's `eclass/` directory.
    pub async fn new(repo: &Repository) -> Result<Self> {
        Self::new_with_cache(repo, Arc::new(papaya::HashMap::new())).await
    }

    /// Create a new shell with a shared eclass AST cache.
    ///
    /// When processing many ebuilds, pass the same `Arc<papaya::HashMap>` to
    /// every shell so that each eclass is parsed at most once.
    pub async fn new_with_cache(
        repo: &Repository,
        eclass_cache: Arc<papaya::HashMap<String, brush_parser::ast::Program>>,
    ) -> Result<Self> {
        let mut shell = Shell::builder()
            .default_builtins(brush_builtins::BuiltinSet::BashMode)
            .do_not_inherit_env(true)
            .profile(ProfileLoadBehavior::Skip)
            .rc(RcLoadBehavior::Skip)
            .parser(ParserImpl::Winnow)
            .build()
            .await
            .map_err(|e| Error::Shell(e.to_string()))?;

        let eclass_dir: Utf8PathBuf = repo.path().join("eclass");
        let eclass_dirs: Vec<Utf8PathBuf> = if eclass_dir.is_dir() {
            vec![eclass_dir]
        } else {
            Vec::new()
        };

        // Register Portage-specific shell functions (die, EXPORT_FUNCTIONS, etc.)
        builtins::register(&mut shell).await?;

        // Register `inherit` with a shared eclass AST cache.
        let inherit_state = inherit::InheritState {
            inherited: Vec::new(),
            cache: eclass_cache,
        };
        shell.register_builtin_with_state(
            "inherit",
            brush_core::builtins::builtin::<inherit::InheritCommand, _>(),
            inherit_state,
        );

        // Register PMS 12.3 utility builtins (has, use, usev, usex, etc.).
        for (name, builtin) in [
            (
                "die",
                brush_core::builtins::builtin::<pms_builtins::DieCommand, _>(),
            ),
            (
                "EXPORT_FUNCTIONS",
                brush_core::builtins::builtin::<pms_builtins::ExportFunctionsCommand, _>(),
            ),
            (
                "has",
                brush_core::builtins::builtin::<pms_builtins::HasCommand, _>(),
            ),
            (
                "hasv",
                brush_core::builtins::builtin::<pms_builtins::HasvCommand, _>(),
            ),
            (
                "hasq",
                brush_core::builtins::builtin::<pms_builtins::HasCommand, _>(),
            ),
            (
                "use",
                brush_core::builtins::builtin::<pms_builtins::UseCommand, _>(),
            ),
            (
                "usev",
                brush_core::builtins::builtin::<pms_builtins::UsevCommand, _>(),
            ),
            (
                "usex",
                brush_core::builtins::builtin::<pms_builtins::UsexCommand, _>(),
            ),
            (
                "use_enable",
                brush_core::builtins::builtin::<pms_builtins::UseEnableCommand, _>(),
            ),
            (
                "use_with",
                brush_core::builtins::builtin::<pms_builtins::UseWithCommand, _>(),
            ),
            (
                "in_iuse",
                brush_core::builtins::builtin::<pms_builtins::InIuseCommand, _>(),
            ),
        ] {
            shell.register_builtin(name, builtin);
        }

        // Register PMS 12.3.14 version manipulation builtins.
        // ver_cut and ver_test are Rust builtins to avoid bash arithmetic
        // issues in array slice expressions (brush limitation).
        // ver_rs is kept as a bash function because brush silently drops
        // empty-string args when calling Rust builtins.
        shell.register_builtin(
            "ver_cut",
            brush_core::builtins::builtin::<ver_funcs::VerCutCommand, _>(),
        );
        shell.register_builtin(
            "ver_rs",
            brush_core::builtins::builtin::<ver_funcs::VerRsCommand, _>(),
        );
        shell.register_builtin(
            "ver_test",
            brush_core::builtins::builtin::<ver_funcs::VerTestCommand, _>(),
        );
        // ver_replacing (EAPI 9): outputs versions being replaced; always
        // empty during metadata extraction.
        shell.register_builtin(
            "ver_replacing",
            brush_core::builtins::builtin::<pms_builtins::VerReplacingCommand, _>(),
        );

        let mut ebuild_shell = EbuildShell {
            shell,
            repo_path: repo.path().to_path_buf(),
            eclass_dirs,
            use_flags: HashSet::new(),
        };
        ebuild_shell.sync_eclass_dirs_var();

        Ok(ebuild_shell)
    }

    /// Append an eclass directory (searched after existing dirs).
    pub fn add_eclass_dir(&mut self, dir: Utf8PathBuf) {
        self.eclass_dirs.push(dir);
        self.sync_eclass_dirs_var();
    }

    /// Prepend an eclass directory (searched before existing dirs).
    ///
    /// Used to add master repository eclass directories so they are
    /// searched before the overlay's own eclasses.
    pub fn prepend_eclass_dir(&mut self, dir: Utf8PathBuf) {
        self.eclass_dirs.insert(0, dir);
        self.sync_eclass_dirs_var();
    }

    /// Update the `__PORTAGE_ECLASS_DIRS` shell variable to reflect the
    /// current set of eclass directories.  Called after any mutation of
    /// [`Self::eclass_dirs`].
    fn sync_eclass_dirs_var(&mut self) {
        let value: String = self
            .eclass_dirs
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(":");
        self.set_var("__PORTAGE_ECLASS_DIRS", &value);
    }

    /// Source an ebuild file and extract its metadata.
    ///
    /// This performs the following steps:
    /// 1. Set PM-provided variables (`CATEGORY`, `PN`, `PV`, `PVR`, `PF`, `P`,
    ///    `FILESDIR`, `WORKDIR`, etc.)
    /// 2. Source the ebuild — the `inherit` shell function handles eclass
    ///    sourcing, line continuations, and nesting automatically
    /// 3. Extract metadata variables from the shell environment
    ///
    /// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
    pub async fn source_ebuild(&mut self, ebuild: &Ebuild) -> Result<EbuildMetadata> {
        // Set PM-provided variables
        let category = ebuild.category();
        let pn = ebuild.name();
        let version = ebuild.version();
        // Use the raw filename version string to preserve leading zeros per PMS §7.2.
        // Version::to_string() normalises numeric components (26.04.0 → 26.4.0).
        let pvr = version.to_string();
        let pr = format!("r{}", version.revision.0);
        let pv = if version.revision.0 > 0 {
            pvr.strip_suffix(&format!("-{pr}"))
                .unwrap_or(&pvr)
                .to_owned()
        } else {
            pvr.clone()
        };
        let p = format!("{pn}-{pv}");
        let pf = format!("{pn}-{pvr}");

        self.set_var("CATEGORY", category);
        self.set_var("PN", pn);
        self.set_var("PV", &pv);
        self.set_var("PR", &pr);
        self.set_var("PVR", &pvr);
        self.set_var("P", &p);
        self.set_var("PF", &pf);

        let filesdir = self.repo_path.join(category).join(pn).join("files");
        self.set_var("FILESDIR", filesdir.as_str());

        // Detect EAPI before sourcing per PMS 7.3.1
        let eapi = ebuild.detect_eapi()?;
        self.set_var("EAPI", &eapi.to_string());

        // Absolute path to the ebuild file (PMS 11.1)
        let ebuild_abs = std::fs::canonicalize(ebuild.path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ebuild.path().to_string());
        self.set_var("EBUILD", &ebuild_abs);

        // Build-directory variables (PMS 11.1)
        // Deterministic placeholders — no temp directories are created.
        let base = format!("/var/tmp/portage/{category}/{pf}");
        let workdir = format!("{base}/work");
        self.set_var("WORKDIR", &workdir);
        self.set_var("S", &format!("{workdir}/{p}"));
        self.set_var("T", &format!("{base}/temp"));
        self.set_var("TMPDIR", &format!("{base}/temp"));
        // Portage uses /tmp as HOME during metadata extraction (depend phase).
        // Using the per-build homedir path causes $HOME expansions in variables
        // like DESCRIPTION to differ from the cached values.
        self.set_var("HOME", "/tmp");
        self.set_var("D", &format!("{base}/image/"));
        self.set_var("DISTDIR", "/var/cache/distfiles");

        // Phase/merge variables (PMS 11.1)
        self.set_var("EBUILD_PHASE", "depend");
        self.set_var("EBUILD_PHASE_FUNC", "");
        self.set_var("ROOT", "/");
        self.set_var("MERGE_TYPE", "source");

        // EAPI 3+ prefix variables (PMS 11.1)
        if eapi >= Eapi::Three {
            self.set_var("EPREFIX", "");
            self.set_var("ED", &format!("{base}/image/"));
            self.set_var("EROOT", "/");
        }

        // EAPI 7+ sysroot variables (PMS 11.1)
        if eapi >= Eapi::Seven {
            self.set_var("SYSROOT", "/");
            self.set_var("ESYSROOT", "/");
            self.set_var("BROOT", "/");
        }

        // PMS 10.2 accumulating variables (EAPI-dependent).
        // Cleared before sourcing so the ebuild's inherit calls populate E_*
        // from scratch.  Combined with ebuild values after sourcing.
        // Mirrors Portage's B_*/E_* pattern in ebuild.sh.
        let accum_vars: &[&str] = if eapi >= Eapi::Eight {
            &[
                "IUSE",
                "REQUIRED_USE",
                "DEPEND",
                "BDEPEND",
                "RDEPEND",
                "PDEPEND",
                "IDEPEND",
                "PROPERTIES",
                "RESTRICT",
            ]
        } else {
            &[
                "IUSE",
                "REQUIRED_USE",
                "DEPEND",
                "BDEPEND",
                "RDEPEND",
                "PDEPEND",
                "IDEPEND",
            ]
        };

        // Clear accumulating vars and their E_* counterparts before sourcing.
        // The ebuild's own inherit calls will repopulate E_* during sourcing.
        let e_accum_pre: &[&str] = if eapi >= Eapi::Eight {
            crate::inherit::E_VARS_ALL
        } else {
            crate::inherit::E_VARS_BASE
        };
        for (&var, &e_var) in accum_vars.iter().zip(e_accum_pre.iter()) {
            self.set_var(var, "");
            self.set_var(e_var, "");
        }
        self.set_var("INHERIT", "");
        self.set_var("INHERITED", "");
        if let Some(state) = self.shell.builtin_state_mut_of::<inherit::InheritCommand>("inherit") {
            state.inherited.clear();
        }

        // EAPI 6+ requires failglob in global scope (PMS 6, Table 6.1).
        // Reset each call so re-used shells get the right state per ebuild.
        if eapi >= Eapi::Six {
            self.run_string("shopt -s failglob").await?;
        } else {
            self.run_string("shopt -u failglob").await?;
        }

        // Source the ebuild — `inherit` is a Rust builtin that accumulates
        // each eclass's contribution into E_{VAR} and restores the var after
        // each eclass (PMS 10.2 / Portage B_*/E_* pattern).
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(
                ebuild.path().as_std_path(),
                std::iter::empty::<&str>(),
                &params,
            )
            .await
            .map_err(|e| Error::Shell(format!("sourcing {}: {e}", ebuild.path())))?;

        // PMS 10.2: combine ebuild-defined values with eclass contributions.
        // After sourcing, `var` holds only what the ebuild set; `E_{var}` holds
        // the total of all eclass contributions.  Append eclass total to ebuild value.
        let e_accum_vars: &[&str] = if eapi >= Eapi::Eight {
            crate::inherit::E_VARS_ALL
        } else {
            crate::inherit::E_VARS_BASE
        };
        for (&var, &e_var) in accum_vars.iter().zip(e_accum_vars.iter()) {
            let ebuild_val = self.get_var(var).unwrap_or_default();
            let eclass_val = self.get_var(e_var).unwrap_or_default();
            let combined = match (ebuild_val.is_empty(), eclass_val.is_empty()) {
                (true, true) => String::new(),
                (true, false) => eclass_val.trim().to_string(),
                (false, true) => ebuild_val,
                (false, false) => format!("{} {}", ebuild_val, eclass_val.trim()),
            };
            self.set_var(var, &combined);
            self.set_var(e_var, ""); // clean up E_*
        }

        // Extract metadata, then override EAPI with the pre-detected value
        // (the authoritative source per PMS 7.3.1)
        let mut metadata = self.extract_metadata()?;
        metadata.eapi = eapi;

        // CacheEntry::parse derives `inherited` from `_eclasses_`, which doesn't
        // exist yet during regen. Read the transitive list directly from the
        // `inherit` builtin's Rust state — no bash-string parsing needed.
        metadata.inherited = self.shell
            .builtin_state_of::<inherit::InheritCommand>("inherit")
            .map(|s| s.inherited.clone())
            .unwrap_or_default();

        Ok(metadata)
    }

    /// Locate portage's script directory under `/usr/lib/portage`.
    ///
    /// Scans for a subdirectory (typically `pythonX.Y`) that contains
    /// `isolated-functions.sh`, and returns the highest-sorted match.
    fn find_portage_bin_path() -> Option<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir("/usr/lib/portage")
            .ok()?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.is_dir() && p.join("isolated-functions.sh").exists() {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        dirs.sort();
        dirs.pop()
    }

    /// Source portage's bash function libraries and set up the build environment.
    ///
    /// Sets `PORTAGE_BIN_PATH`, prepends `ebuild-helpers/` to `PATH`, configures
    /// a writable `DISTDIR`, passes through build-tool variables (`CFLAGS`,
    /// `MAKEOPTS`, …) from the caller's environment, and sources:
    ///
    /// - `isolated-functions.sh` — `einfo`, `ewarn`, `eerror`, `ebegin`, `eend`, `die`, …
    /// - `phase-functions.sh`   — `__ebuild_phase_funcs`, default phase implementations
    /// - `phase-helpers.sh`     — `econf`, `unpack`, `insinto`, `into`, `use_enable`, …
    ///
    /// Failures to source individual files are warned rather than fatal so the
    /// caller can still attempt to run phases against a partial environment.
    pub async fn init_build_env(&mut self) -> Result<()> {
        let bin_path = Self::find_portage_bin_path()
            .ok_or_else(|| Error::Shell("portage not found under /usr/lib/portage".to_string()))?;

        self.set_var("PORTAGE_BIN_PATH", &bin_path.to_string_lossy());

        // Prepend ebuild-helpers to PATH so do*, new*, e* commands resolve.
        let helpers = bin_path.join("ebuild-helpers");
        let cur_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
        self.set_var("PATH", &format!("{}:{cur_path}", helpers.display()));

        // Writable DISTDIR: honour env override, fall back to ~/.cache/distfiles.
        let distdir = std::env::var("DISTDIR").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            format!("{home}/.cache/distfiles")
        });
        std::fs::create_dir_all(&distdir).ok();
        self.set_var("DISTDIR", &distdir);

        // Pass through build-tool variables from the caller's environment.
        for var in &[
            "MAKEOPTS", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS",
            "CC", "CXX", "AR", "RANLIB", "NM", "STRIP", "PKG_CONFIG",
        ] {
            if let Ok(val) = std::env::var(var) {
                self.set_var(var, &val);
            }
        }

        // Source portage's function libraries in the same order as ebuild.sh.
        let params = self.shell.default_exec_params();
        for lib in &["isolated-functions.sh", "phase-functions.sh", "phase-helpers.sh"] {
            let path = bin_path.join(lib);
            if path.exists() {
                if let Err(e) = self.shell
                    .source_script(path.as_path(), std::iter::empty::<&str>(), &params)
                    .await
                {
                    eprintln!("warning: could not source {lib}: {e}");
                }
            }
        }

        Ok(())
    }

    /// Source an ebuild and run a single phase function.
    ///
    /// Creates the standard build directories under `work_root` if they don't
    /// exist, sets all PMS environment variables, sources the ebuild (which
    /// triggers `inherit` and populates eclass functions), then calls the
    /// phase function if it is defined.
    ///
    /// Unlike [`source_ebuild`], no metadata extraction is performed.  Output
    /// from the phase (stdout/stderr) is passed through to the caller's
    /// terminal.
    ///
    /// # Arguments
    /// * `ebuild`    – the ebuild to source
    /// * `phase`     – portage phase name (`"compile"`, `"install"`, …) or raw
    ///                 function name (`"src_compile"`)
    /// * `work_root` – root for build dirs; `work/`, `temp/`, `image/` are
    ///                 created beneath it
    pub async fn run_phase(&mut self, ebuild: &Ebuild, phase: &str, work_root: &Path) -> Result<()> {
        let category = ebuild.category();
        let pn = ebuild.name();
        let version = ebuild.version();
        let pvr = version.to_string();
        let pr = format!("r{}", version.revision.0);
        let pv = if version.revision.0 > 0 {
            pvr.strip_suffix(&format!("-{pr}")).unwrap_or(&pvr).to_owned()
        } else {
            pvr.clone()
        };
        let p = format!("{pn}-{pv}");
        let pf = format!("{pn}-{pvr}");

        self.set_var("CATEGORY", category);
        self.set_var("PN", pn);
        self.set_var("PV", &pv);
        self.set_var("PR", &pr);
        self.set_var("PVR", &pvr);
        self.set_var("P", &p);
        self.set_var("PF", &pf);

        let filesdir = self.repo_path.join(category).join(pn).join("files");
        self.set_var("FILESDIR", filesdir.as_str());

        let eapi = ebuild.detect_eapi()?;
        self.set_var("EAPI", &eapi.to_string());

        let ebuild_abs = std::fs::canonicalize(ebuild.path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ebuild.path().to_string());
        self.set_var("EBUILD", &ebuild_abs);

        // Source portage's function libraries and configure build environment.
        // Must happen after EAPI is set so phase-functions.sh registers the
        // right EAPI-specific defaults.
        self.init_build_env().await?;

        // Create and set real build directories.
        let workdir = work_root.join("work");
        let t = work_root.join("temp");
        let d = work_root.join("image");
        let homedir = work_root.join("homedir");
        for dir in [&workdir, &t, &d, &homedir] {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Shell(format!("creating {}: {e}", dir.display())))?;
        }
        self.set_var("WORKDIR", &workdir.to_string_lossy());
        self.set_var("S", &workdir.join(&p).to_string_lossy());
        self.set_var("T", &t.to_string_lossy());
        self.set_var("TMPDIR", &t.to_string_lossy());
        self.set_var("HOME", &homedir.to_string_lossy());
        self.set_var("D", &format!("{}/", d.display()));
        self.set_var("DISTDIR", "/var/cache/distfiles");

        // Phase and merge variables.
        let (phase_val, func_name) = phase_to_func(phase);
        self.set_var("EBUILD_PHASE", phase_val);
        self.set_var("EBUILD_PHASE_FUNC", func_name);
        self.set_var("ROOT", "/");
        self.set_var("MERGE_TYPE", "source");

        if eapi >= Eapi::Three {
            self.set_var("EPREFIX", "");
            self.set_var("ED", &format!("{}/", d.display()));
            self.set_var("EROOT", "/");
        }
        if eapi >= Eapi::Seven {
            self.set_var("SYSROOT", "/");
            self.set_var("ESYSROOT", "/");
            self.set_var("BROOT", "/");
        }

        // Clear eclass accumulation state (same as source_ebuild).
        let accum_vars: &[&str] = if eapi >= Eapi::Eight {
            &["IUSE","REQUIRED_USE","DEPEND","BDEPEND","RDEPEND","PDEPEND","IDEPEND","PROPERTIES","RESTRICT"]
        } else {
            &["IUSE","REQUIRED_USE","DEPEND","BDEPEND","RDEPEND","PDEPEND","IDEPEND"]
        };
        let e_vars: &[&str] = if eapi >= Eapi::Eight {
            inherit::E_VARS_ALL
        } else {
            inherit::E_VARS_BASE
        };
        for (&var, &e_var) in accum_vars.iter().zip(e_vars.iter()) {
            self.set_var(var, "");
            self.set_var(e_var, "");
        }
        self.set_var("INHERIT", "");
        self.set_var("INHERITED", "");
        if let Some(state) = self.shell.builtin_state_mut_of::<inherit::InheritCommand>("inherit") {
            state.inherited.clear();
        }

        if eapi >= Eapi::Six {
            self.run_string("shopt -s failglob").await?;
        } else {
            self.run_string("shopt -u failglob").await?;
        }

        // Source the ebuild — defines all phase functions and global variables.
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(ebuild.path().as_std_path(), std::iter::empty::<&str>(), &params)
            .await
            .map_err(|e| Error::Shell(format!("sourcing {}: {e}", ebuild.path())))?;

        // Combine eclass E_* contributions with ebuild-defined values (PMS 10.2).
        for (&var, &e_var) in accum_vars.iter().zip(e_vars.iter()) {
            let ebuild_val = self.get_var(var).unwrap_or_default();
            let eclass_val = self.get_var(e_var).unwrap_or_default();
            let combined = match (ebuild_val.is_empty(), eclass_val.is_empty()) {
                (true, true) => String::new(),
                (true, false) => eclass_val.trim().to_string(),
                (false, true) => ebuild_val,
                (false, false) => format!("{} {}", ebuild_val, eclass_val.trim()),
            };
            self.set_var(var, &combined);
            self.set_var(e_var, "");
        }

        // Wire up `default` and any missing EAPI default implementations
        // (e.g. src_compile → __eapi2_src_compile) for the current phase.
        if self.shell.funcs().get("__ebuild_phase_funcs").is_some() {
            self.run_string(&format!(
                "__ebuild_phase_funcs {eapi} {func_name}"
            ))
            .await
            .ok();
        }

        // Run the phase function if it is defined; warn otherwise.
        if self.shell.funcs().get(func_name).is_some() {
            self.run_string(func_name).await?;
        } else {
            eprintln!("warning: {func_name} not defined, nothing to do");
        }

        Ok(())
    }

    /// Source an eclass by name.
    ///
    /// Searches the configured eclass directories in order.
    pub async fn source_eclass(&mut self, name: &str) -> Result<()> {
        let filename = format!("{name}.eclass");
        for dir in &self.eclass_dirs {
            let path: Utf8PathBuf = dir.join(&filename);
            if path.is_file() {
                let params = self.shell.default_exec_params();
                self.shell
                    .source_script(path.as_std_path(), std::iter::empty::<&str>(), &params)
                    .await
                    .map_err(|e| Error::Shell(format!("sourcing eclass {name}: {e}")))?;
                return Ok(());
            }
        }
        Err(Error::Shell(format!("eclass not found: {name}")))
    }

    /// Source a `make.defaults` file.
    ///
    /// Variable assignments (with `${VAR}` expansion) are evaluated in the
    /// shell environment.
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#makedefaults).
    pub async fn source_make_defaults(&mut self, path: &Path) -> Result<()> {
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(path, std::iter::empty::<&str>(), &params)
            .await
            .map_err(|e| Error::Shell(format!("sourcing make.defaults {}: {e}", path.display())))?;
        Ok(())
    }

    /// Resolve the path of a named eclass by searching the configured eclass directories.
    pub fn eclass_path(&self, name: &str) -> Option<Utf8PathBuf> {
        let filename = format!("{name}.eclass");
        self.eclass_dirs
            .iter()
            .map(|dir| dir.join(&filename))
            .find(|p| p.is_file())
    }

    /// Read a variable from the shell environment.
    pub fn get_var(&self, name: &str) -> Option<String> {
        self.shell.env_str(name).map(|cow| cow.into_owned())
    }

    /// Set a variable in the shell environment.
    fn set_var(&mut self, name: &str, value: &str) {
        let _ = self.shell.set_env_global(
            name,
            ShellVariable::new(ShellValue::String(value.to_string())),
        );
    }

    /// Run a bash script string directly in the shell without writing a temporary file.
    pub async fn run_string(&mut self, script: &str) -> Result<()> {
        let params = self.shell.default_exec_params();
        let source_info = SourceInfo::from("inline");
        self.shell
            .run_string(script, &source_info, &params)
            .await
            .map_err(|e| Error::Shell(format!("run_string: {e}")))?;
        Ok(())
    }

    /// Set the active USE flags for this shell session.
    ///
    /// These flags will be used by the `use()`, `usev()`, `usex()` functions
    /// when sourcing ebuilds and eclasses.
    ///
    /// # Example
    /// ```no_run
    /// use portage_repo::Repository;
    ///
    /// # async fn example() {
    /// let repo = Repository::open("/var/db/repos/gentoo").unwrap();
    /// let mut shell = repo.shell().await.unwrap();
    /// shell.set_use_flags(&["ssl", "gtk", "-doc"]).unwrap();
    /// # }
    /// ```
    pub fn set_use_flags(&mut self, flags: &[&str]) -> Result<()> {
        let mut new_flags = HashSet::new();

        for flag in flags {
            let flag_str = flag.trim();
            if flag_str.is_empty() {
                continue;
            }

            let (flag_name, enabled) = if let Some(stripped) = flag_str.strip_prefix('-') {
                (stripped.to_string(), false)
            } else if let Some(stripped) = flag_str.strip_prefix('+') {
                (stripped.to_string(), true)
            } else {
                (flag_str.to_string(), true)
            };

            if enabled {
                new_flags.insert(flag_name);
            } else {
                new_flags.remove(&flag_name);
            }
        }

        self.use_flags = new_flags;

        // Update the USE environment variable
        let use_flags = self.use_flags_string();
        if !use_flags.is_empty() {
            self.set_var("USE", &use_flags);
        } else {
            self.set_var("USE", "");
        }

        Ok(())
    }

    /// Get the current USE flags as a space-separated string.
    ///
    /// This can be used to set the `USE` environment variable in the shell.
    pub fn use_flags_string(&self) -> String {
        let mut flags: Vec<_> = self.use_flags.iter().cloned().collect();
        flags.sort();
        flags.join(" ")
    }

    /// Extract metadata from shell variables into a `CacheEntry`-compatible string
    /// and parse it via portage-metadata.
    fn extract_metadata(&self) -> Result<EbuildMetadata> {
        // Collect (key, value) pairs directly from the shell environment,
        // using Cow<str> to avoid cloning when the value needs no normalization.
        let pairs: Vec<(&str, std::borrow::Cow<str>)> = METADATA_VARS
            .iter()
            .filter_map(|&var| {
                let value = self.shell.env_str(var)?;
                if value.is_empty() {
                    return None;
                }
                // Normalize embedded newlines/tabs to spaces (heredoc values).
                let normalized = if var == "DESCRIPTION" {
                    std::borrow::Cow::Owned(itertools::join(value.split_whitespace(), " "))
                } else if value.bytes().any(|b| matches!(b, b'\n' | b'\r' | b'\t')) {
                    std::borrow::Cow::Owned(itertools::join(value.split_whitespace(), " "))
                } else {
                    value
                };
                if normalized.is_empty() {
                    return None;
                }
                Some((var, normalized))
            })
            .collect();

        let entry = portage_metadata::CacheEntry::from_kv_pairs(
            pairs.iter().map(|(k, v)| (*k, v.as_ref())),
        )?;

        // Compute DEFINED_PHASES by inspecting which phase functions are
        // defined in the shell after sourcing (PMS 7.4).
        let mut defined_phases: Vec<Phase> = PHASE_FUNCTIONS
            .iter()
            .filter(|(name, _)| self.shell.funcs().get(name).is_some())
            .map(|(_, phase)| *phase)
            .collect();
        // Sort alphabetically by short name to match Portage's cache format.
        defined_phases.sort_by_key(|p| p.as_str());

        let mut metadata = entry.metadata;
        metadata.defined_phases = defined_phases;
        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_use_flags() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().to_path_buf();

        // Create a minimal repository structure
        std::fs::create_dir_all(repo_path.join("metadata")).unwrap();
        std::fs::create_dir_all(repo_path.join("profiles")).unwrap();
        std::fs::create_dir_all(repo_path.join("eclass")).unwrap();

        // Write minimal layout.conf
        std::fs::write(
            repo_path.join("metadata").join("layout.conf"),
            "masters = \ncache-formats = md5-dict\n",
        )
        .unwrap();

        // Write repo_name
        std::fs::write(repo_path.join("profiles").join("repo_name"), "test-repo\n").unwrap();

        let repo = Repository::open(&repo_path).unwrap();
        let mut shell = repo.shell().await.unwrap();

        // Test setting USE flags
        shell.set_use_flags(&["ssl", "gtk", "-doc"]).unwrap();
        assert_eq!(shell.use_flags_string(), "gtk ssl");

        // Test that USE environment variable is set
        let use_env = shell.get_var("USE").unwrap_or_default();
        assert!(use_env.contains("ssl"));
        assert!(use_env.contains("gtk"));
        assert!(!use_env.contains("doc"));
    }
}
