use std::path::PathBuf;

use portage_atom::Cpv;

use crate::error::Result;
use crate::util;

/// A single ebuild file within a package directory.
///
/// This is intentionally thin — it represents the `.ebuild` file on disk.
/// Metadata extraction goes through [`EbuildShell`](crate::EbuildShell).
///
/// See [PMS 4](https://projects.gentoo.org/pms/latest/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct Ebuild {
    cpv: Cpv,
    path: PathBuf,
}

impl Ebuild {
    pub(crate) fn new(cpv: Cpv, path: PathBuf) -> Self {
        Self { cpv, path }
    }

    /// The full category/package-version atom.
    pub fn cpv(&self) -> &Cpv {
        &self.cpv
    }

    /// The category name.
    pub fn category(&self) -> &str {
        self.cpv.category()
    }

    /// The package name (without version).
    pub fn name(&self) -> &str {
        self.cpv.package()
    }

    /// The version.
    pub fn version(&self) -> &portage_atom::Version {
        &self.cpv.version
    }

    /// Absolute path to the `.ebuild` file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Read the raw ebuild file content.
    pub fn read_raw(&self) -> Result<String> {
        util::read_to_string(&self.path)
    }
}
