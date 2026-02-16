use std::path::{Path, PathBuf};

use brush_builtins::ShellBuilderExt;
use brush_core::parser::ParserImpl;
use brush_core::{ProfileLoadBehavior, RcLoadBehavior, Shell, ShellValue, ShellVariable};
use portage_metadata::{EbuildMetadata, Phase};

use crate::builtins;
use crate::ebuild::Ebuild;
use crate::error::{Error, Result};
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

        // Register Portage-specific shell functions (inherit, die, etc.)
        builtins::register(&mut shell).await?;

        let mut ebuild_shell = EbuildShell {
            shell,
            repo_path: repo.path().to_path_buf(),
            eclass_dirs,
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
        let pv = version.to_string();
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

        // Source the ebuild — `inherit` is a shell function that handles
        // eclass sourcing, line continuations, and nesting naturally.
        let params = self.shell.default_exec_params();
        self.shell
            .source_script(ebuild.path(), std::iter::empty::<&str>(), &params)
            .await
            .map_err(|e| Error::Shell(format!("sourcing {}: {e}", ebuild.path().display())))?;

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

    /// Extract metadata from shell variables into a `CacheEntry`-compatible string
    /// and parse it via portage-metadata.
    fn extract_metadata(&self) -> Result<EbuildMetadata> {
        let mut cache_lines = Vec::new();
        for &var in METADATA_VARS {
            if let Some(value) = self.get_var(var)
                && !value.is_empty()
            {
                cache_lines.push(format!("{var}={value}"));
            }
        }

        let cache_str = cache_lines.join("\n");
        let entry = portage_metadata::CacheEntry::parse(&cache_str)?;

        // Compute DEFINED_PHASES by inspecting which phase functions are
        // defined in the shell after sourcing (PMS 7.4).
        let defined_phases: Vec<Phase> = PHASE_FUNCTIONS
            .iter()
            .filter(|(name, _)| self.shell.funcs().get(name).is_some())
            .map(|(_, phase)| *phase)
            .collect();

        let mut metadata = entry.metadata;
        metadata.defined_phases = defined_phases;
        Ok(metadata)
    }
}
