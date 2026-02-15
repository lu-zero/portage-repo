use std::path::PathBuf;

use portage_atom::{Cpn, Cpv};

use crate::ebuild::Ebuild;
use crate::error::Result;
use crate::util;

/// A package directory within a category.
///
/// For example, `dev-lang/rust/` contains ebuild files like `rust-1.75.0.ebuild`.
///
/// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct Package {
    cpn: Cpn,
    path: PathBuf,
}

impl Package {
    pub(crate) fn new(category: &str, name: String, path: PathBuf) -> Self {
        Self {
            cpn: Cpn::new(category, &name),
            path,
        }
    }

    /// The category/package name atom.
    pub fn cpn(&self) -> &Cpn {
        &self.cpn
    }

    /// The category name.
    pub fn category(&self) -> &str {
        &self.cpn.category
    }

    /// The package name.
    pub fn name(&self) -> &str {
        &self.cpn.package
    }

    /// Absolute path to the package directory.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// List all ebuilds in this package directory, sorted by version.
    ///
    /// Parses each `*.ebuild` filename into a [`Cpv`] by stripping the `.ebuild`
    /// extension and parsing `category/stem` as a versioned package atom.
    pub fn ebuilds(&self) -> Result<Vec<Ebuild>> {
        let entries = match std::fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&self.path, e)),
        };

        let mut ebuilds = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| util::io_err(&self.path, e))?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".ebuild") {
                let cpv_str = format!("{}/{stem}", self.cpn.category);
                if let Ok(cpv) = Cpv::parse(&cpv_str) {
                    ebuilds.push(Ebuild::new(cpv, entry.path()));
                }
            }
        }
        ebuilds.sort_by(|a, b| a.cpv().cmp(b.cpv()));
        Ok(ebuilds)
    }

    /// Look up a specific ebuild by version string.
    ///
    /// The `version` parameter is the version portion only (e.g. `"1.75.0"`),
    /// not the full filename.
    pub fn ebuild(&self, version: &str) -> Result<Option<Ebuild>> {
        let cpv_str = format!("{}/{}-{version}", self.cpn.category, self.cpn.package);
        let cpv = Cpv::parse(&cpv_str)?;
        let filename = format!("{}-{version}.ebuild", self.cpn.package);
        let path = self.path.join(&filename);
        if path.is_file() {
            Ok(Some(Ebuild::new(cpv, path)))
        } else {
            Ok(None)
        }
    }

    /// Whether a `Manifest` file exists.
    pub fn has_manifest(&self) -> bool {
        self.path.join("Manifest").is_file()
    }

    /// Whether a `metadata.xml` file exists.
    pub fn has_metadata_xml(&self) -> bool {
        self.path.join("metadata.xml").is_file()
    }
}
