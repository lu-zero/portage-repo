use std::path::PathBuf;

use crate::error::Result;
use crate::package::Package;
use crate::util;

/// A category directory within an ebuild repository.
///
/// Represents a directory such as `dev-lang/` containing package directories.
///
/// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct Category {
    name: String,
    path: PathBuf,
}

impl Category {
    pub(crate) fn new(name: String, path: PathBuf) -> Self {
        Self { name, path }
    }

    /// The category name (e.g. `dev-lang`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Absolute path to the category directory.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Whether the category directory exists on disk.
    pub fn exists(&self) -> bool {
        self.path.is_dir()
    }

    /// List all packages in this category.
    ///
    /// Returns package directories sorted by name. Non-directory entries and
    /// dotfiles are skipped.
    pub fn packages(&self) -> Result<Vec<Package>> {
        let entries = match std::fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&self.path, e)),
        };

        let mut packages = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| util::io_err(&self.path, e))?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                packages.push(Package::new(&self.name, name.into_owned(), path));
            }
        }
        packages.sort_by(|a, b| a.name().cmp(b.name()));
        Ok(packages)
    }

    /// Look up a specific package by name.
    pub fn package(&self, name: &str) -> Option<Package> {
        let path = self.path.join(name);
        if path.is_dir() {
            Some(Package::new(&self.name, name.to_string(), path))
        } else {
            None
        }
    }
}
