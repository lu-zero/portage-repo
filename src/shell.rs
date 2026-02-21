use std::collections::HashSet;
use std::path::{Path, PathBuf};

use brush_builtins::ShellBuilderExt;
use brush_core::parser::ParserImpl;
use brush_core::{ProfileLoadBehavior, RcLoadBehavior, Shell, ShellValue, ShellVariable, SourceInfo};
use portage_metadata::{Eapi, EbuildMetadata, Phase};

use crate::builtins;
use crate::ebuild::Ebuild;
use crate::error::{Error, Result};
use crate::inherit;
use crate::repository::Repository;

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
    "INHERITED",
];

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
    repo_path: PathBuf,
    eclass_dirs: Vec<PathBuf>,
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
        let mut shell = Shell::builder()
            .default_builtins(brush_builtins::BuiltinSet::BashMode)
            .do_not_inherit_env(true)
            .profile(ProfileLoadBehavior::Skip)
            .rc(RcLoadBehavior::Skip)
            .parser(ParserImpl::Winnow)
            .build()
            .await
            .map_err(|e| Error::Shell(e.to_string()))?;

        let eclass_dir = repo.path().join("eclass");
        let eclass_dirs = if eclass_dir.is_dir() {
            vec![eclass_dir]
        } else {
            Vec::new()
        };

        // Register Portage-specific shell functions (die, EXPORT_FUNCTIONS, etc.)
        builtins::register(&mut shell).await?;

        // Register `inherit` as a Rust builtin (avoids brush-core scoping bug
        // where arrays become invisible after nested source calls in functions).
        shell.register_builtin(
            "inherit",
            brush_core::builtins::builtin::<inherit::InheritCommand, _>(),
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
    pub fn add_eclass_dir(&mut self, dir: PathBuf) {
        self.eclass_dirs.push(dir);
        self.sync_eclass_dirs_var();
    }

    /// Prepend an eclass directory (searched before existing dirs).
    ///
    /// Used to add master repository eclass directories so they are
    /// searched before the overlay's own eclasses.
    pub fn prepend_eclass_dir(&mut self, dir: PathBuf) {
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
            .map(|p| p.to_string_lossy())
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
        let pv = version.base().to_string();
        let pvr = if version.revision.0 > 0 {
            format!("{pv}-r{}", version.revision.0)
        } else {
            pv.clone()
        };
        let pr = format!("r{}", version.revision.0);
        let p = format!("{pn}-{pv}");
        let pf = format!("{pn}-{pvr}");

        self.set_var("CATEGORY", category);
        self.set_var("PN", pn);
        self.set_var("PV", &pv);
        self.set_var("PR", &pr);
        self.set_var("PVR", &pvr);
        self.set_var("P", &p);
        self.set_var("PF", &pf);

        let filesdir = self
            .repo_path
            .join(category)
            .join(pn)
            .join("files")
            .to_string_lossy()
            .into_owned();
        self.set_var("FILESDIR", &filesdir);

        // Detect EAPI before sourcing per PMS 7.3.1
        let eapi = ebuild.detect_eapi()?;
        self.set_var("EAPI", &eapi.to_string());

        // Absolute path to the ebuild file (PMS 11.1)
        let ebuild_path =
            std::fs::canonicalize(ebuild.path()).unwrap_or_else(|_| ebuild.path().clone());
        self.set_var("EBUILD", &ebuild_path.to_string_lossy());

        // Build-directory variables (PMS 11.1)
        // Deterministic placeholders — no temp directories are created.
        let base = format!("/var/tmp/portage/{category}/{pf}");
        let workdir = format!("{base}/work");
        self.set_var("WORKDIR", &workdir);
        self.set_var("S", &format!("{workdir}/{p}"));
        self.set_var("T", &format!("{base}/temp"));
        self.set_var("TMPDIR", &format!("{base}/temp"));
        self.set_var("HOME", &format!("{base}/homedir"));
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
        for &var in accum_vars {
            self.set_var(var, "");
            self.set_var(&format!("E_{var}"), "");
        }
        self.set_var("INHERITED", "");

        // Source the ebuild — `inherit` is a Rust builtin that accumulates
        // each eclass's contribution into E_{VAR} and restores the var after
        // each eclass (PMS 10.2 / Portage B_*/E_* pattern).
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(ebuild.path(), std::iter::empty::<&str>(), &params)
            .await
            .map_err(|e| Error::Shell(format!("sourcing {}: {e}", ebuild.path().display())))?;

        // PMS 10.2: combine ebuild-defined values with eclass contributions.
        // After sourcing, `var` holds only what the ebuild set; `E_{var}` holds
        // the total of all eclass contributions.  Append eclass total to ebuild value.
        for &var in accum_vars {
            let ebuild_val = self.get_var(var).unwrap_or_default();
            let e_var = format!("E_{var}");
            let eclass_val = self.get_var(&e_var).unwrap_or_default();
            let combined = match (ebuild_val.is_empty(), eclass_val.is_empty()) {
                (true, true) => String::new(),
                (true, false) => eclass_val.trim().to_string(),
                (false, true) => ebuild_val,
                (false, false) => format!("{} {}", ebuild_val, eclass_val.trim()),
            };
            self.set_var(var, &combined);
            self.set_var(&e_var, ""); // clean up E_*
        }

        // Extract metadata, then override EAPI with the pre-detected value
        // (the authoritative source per PMS 7.3.1)
        let mut metadata = self.extract_metadata()?;
        metadata.eapi = eapi;
        Ok(metadata)
    }

    /// Source an eclass by name.
    ///
    /// Searches the configured eclass directories in order.
    pub async fn source_eclass(&mut self, name: &str) -> Result<()> {
        let filename = format!("{name}.eclass");
        for dir in &self.eclass_dirs {
            let path = dir.join(&filename);
            if path.is_file() {
                let params = self.shell.default_exec_params();
                self.shell
                    .source_script(&path, std::iter::empty::<&str>(), &params)
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
        let mut cache_lines = Vec::new();
        for &var in METADATA_VARS {
            if let Some(value) = self.get_var(var)
                && !value.is_empty()
            {
                // Normalize whitespace: bash values may contain embedded
                // newlines and tabs from heredocs / multi-line assignments,
                // but the portage cache format expects single-line values
                // with space-separated atoms.
                let normalized: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
                if !normalized.is_empty() {
                    cache_lines.push(format!("{var}={normalized}"));
                }
            }
        }

        let cache_str = cache_lines.join("\n");
        let entry = portage_metadata::CacheEntry::parse(&cache_str)?;

        // Compute DEFINED_PHASES by inspecting which phase functions are
        // defined in the shell after sourcing (PMS 7.4).
        let mut defined_phases: Vec<Phase> = PHASE_FUNCTIONS
            .iter()
            .filter(|(name, _)| self.shell.funcs().get(name).is_some())
            .map(|(_, phase)| *phase)
            .collect();
        // Sort alphabetically by short name to match Portage's cache format.
        defined_phases.sort_by_key(|p| p.to_string());

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
