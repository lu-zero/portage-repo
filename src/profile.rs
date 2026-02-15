use std::path::{Path, PathBuf};

use portage_atom::Dep;
use portage_metadata::Eapi;

use crate::error::{Error, Result};
use crate::shell::EbuildShell;
use crate::util;

/// Stability status of a profile.
///
/// PMS allows repositories to define arbitrary status values beyond the
/// well-known `stable`, `dev`, and `exp`.
///
/// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProfileStatus {
    /// Stable profile.
    Stable,
    /// Development profile.
    Dev,
    /// Experimental profile.
    Exp,
    /// A repository-defined status value not covered by the well-known variants.
    Other(String),
}

impl ProfileStatus {
    fn parse(s: &str) -> Self {
        match s {
            "stable" => ProfileStatus::Stable,
            "dev" => ProfileStatus::Dev,
            "exp" => ProfileStatus::Exp,
            other => ProfileStatus::Other(other.to_string()),
        }
    }
}

/// A profile entry from `profiles/profiles.desc`.
///
/// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone)]
pub struct ProfileDesc {
    /// Architecture keyword (e.g. `amd64`).
    pub arch: String,
    /// Path relative to `profiles/` (e.g. `default/linux/amd64/23.0`).
    pub path: String,
    /// Stability status.
    pub status: ProfileStatus,
}

impl ProfileDesc {
    /// Parse a single line from `profiles.desc`.
    ///
    /// Format: `arch path status`
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(Error::InvalidProfile(format!(
                "expected 'arch path status', got: {line}"
            )));
        }
        Ok(ProfileDesc {
            arch: parts[0].to_string(),
            path: parts[1].to_string(),
            status: ProfileStatus::parse(parts[2]),
        })
    }
}

/// A profile directory.
///
/// Profiles contain stacked configuration files that control default
/// USE flags, package masking, keywords, and more.
///
/// See [PMS 5 — Profiles](https://projects.gentoo.org/pms/9/pms.html#profiles).
#[derive(Debug, Clone)]
pub struct Profile {
    path: PathBuf,
    eapi: Eapi,
}

impl Profile {
    /// Open a profile at the given directory path.
    pub fn open(path: PathBuf) -> Result<Self> {
        let eapi_str = util::read_single_line(&path.join("eapi"))?;
        let eapi = match eapi_str {
            Some(s) => s
                .parse::<Eapi>()
                .map_err(|e| Error::InvalidProfile(format!("bad EAPI: {e}")))?,
            None => Eapi::Zero,
        };
        Ok(Profile { path, eapi })
    }

    /// Absolute path to the profile directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The EAPI declared by this profile (from the `eapi` file).
    pub fn eapi(&self) -> Eapi {
        self.eapi
    }

    /// Parse the `parent` file to get parent profile paths.
    ///
    /// Paths are relative to this profile directory and resolved to absolute paths.
    pub fn parents(&self) -> Result<Vec<PathBuf>> {
        let lines = util::read_lines(&self.path.join("parent"))?;
        Ok(lines.iter().map(|l| self.path.join(l)).collect())
    }

    /// Parse the `packages` file.
    ///
    /// Returns `(is_system, dep)` pairs. Lines prefixed with `*` indicate
    /// system packages.
    ///
    /// See [PMS 5.2.6](https://projects.gentoo.org/pms/9/pms.html#packages).
    pub fn packages(&self) -> Result<Vec<(bool, Dep)>> {
        let lines = util::read_lines(&self.path.join("packages"))?;
        let mut result = Vec::new();
        for line in lines {
            let (is_system, atom_str) = if let Some(rest) = line.strip_prefix('*') {
                (true, rest.trim())
            } else {
                (false, line.as_str())
            };
            let dep = Dep::parse(atom_str)?;
            result.push((is_system, dep));
        }
        Ok(result)
    }

    /// Parse `package.mask`.
    ///
    /// See [PMS 5.2.8](https://projects.gentoo.org/pms/9/pms.html#packagemask).
    pub fn package_mask(&self) -> Result<Vec<Dep>> {
        parse_atom_list(&self.path.join("package.mask"))
    }

    /// Parse `package.use`.
    ///
    /// Returns `(dep, [flags...])` pairs.
    pub fn package_use(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use"))
    }

    /// Parse `use.force`.
    pub fn use_force(&self) -> Result<Vec<String>> {
        util::read_lines(&self.path.join("use.force"))
    }

    /// Parse `use.mask`.
    pub fn use_mask(&self) -> Result<Vec<String>> {
        util::read_lines(&self.path.join("use.mask"))
    }

    /// Parse `use.stable.force`.
    pub fn use_stable_force(&self) -> Result<Vec<String>> {
        util::read_lines(&self.path.join("use.stable.force"))
    }

    /// Parse `use.stable.mask`.
    pub fn use_stable_mask(&self) -> Result<Vec<String>> {
        util::read_lines(&self.path.join("use.stable.mask"))
    }

    /// Parse `package.use.force`.
    pub fn package_use_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.force"))
    }

    /// Parse `package.use.mask`.
    pub fn package_use_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.mask"))
    }

    /// Parse `package.use.stable.force`.
    pub fn package_use_stable_force(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.stable.force"))
    }

    /// Parse `package.use.stable.mask`.
    pub fn package_use_stable_mask(&self) -> Result<Vec<(Dep, Vec<String>)>> {
        parse_atom_flags_list(&self.path.join("package.use.stable.mask"))
    }

    /// Source `make.defaults` through a brush shell.
    ///
    /// Variable assignments (including `${VAR}` expansions) are evaluated in the
    /// shell environment. After this call, the shell's environment contains the
    /// variables defined in `make.defaults`.
    ///
    /// See [PMS 5.2.4](https://projects.gentoo.org/pms/9/pms.html#makedefaults).
    pub async fn make_defaults(&self, shell: &mut EbuildShell) -> Result<()> {
        let path = self.path.join("make.defaults");
        if path.is_file() {
            shell.source_make_defaults(&path).await?;
        }
        Ok(())
    }
}

/// Parse a file containing one dependency atom per line.
fn parse_atom_list(path: &Path) -> Result<Vec<Dep>> {
    let lines = util::read_lines(path)?;
    let mut result = Vec::new();
    for line in lines {
        result.push(Dep::parse(&line)?);
    }
    Ok(result)
}

/// Parse a file containing `atom flag1 flag2 ...` per line.
fn parse_atom_flags_list(path: &Path) -> Result<Vec<(Dep, Vec<String>)>> {
    let lines = util::read_lines(path)?;
    let mut result = Vec::new();
    for line in lines {
        let mut parts = line.split_whitespace();
        if let Some(atom_str) = parts.next() {
            let dep = Dep::parse(atom_str)?;
            let flags: Vec<String> = parts.map(String::from).collect();
            result.push((dep, flags));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_desc_line() {
        let desc = ProfileDesc::parse("amd64 default/linux/amd64/23.0 stable").unwrap();
        assert_eq!(desc.arch, "amd64");
        assert_eq!(desc.path, "default/linux/amd64/23.0");
        assert_eq!(desc.status, ProfileStatus::Stable);
    }

    #[test]
    fn parse_profile_desc_dev() {
        let desc = ProfileDesc::parse("arm64 default/linux/arm64/23.0 dev").unwrap();
        assert_eq!(desc.status, ProfileStatus::Dev);
    }

    #[test]
    fn parse_profile_desc_exp() {
        let desc = ProfileDesc::parse("riscv default/linux/riscv/23.0 exp").unwrap();
        assert_eq!(desc.status, ProfileStatus::Exp);
    }

    #[test]
    fn parse_profile_desc_other_status() {
        let desc = ProfileDesc::parse("x86 some/path testing").unwrap();
        assert_eq!(desc.status, ProfileStatus::Other("testing".to_string()));
    }

    #[test]
    fn parse_profile_desc_too_few_fields() {
        assert!(ProfileDesc::parse("amd64 some/path").is_err());
    }
}
