use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jwalk::WalkDir;
use portage_atom::{Cpn, Cpv, Dep};
use portage_metadata::{CacheEntry, Eapi};

/// A single package-move or slot-move entry from `profiles/updates/`.
///
/// See [PMS 4.4.4](https://projects.gentoo.org/pms/9/pms.html#profiles-updates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileUpdate {
    /// `move <old> <new>` — package renamed.
    Move {
        /// Old category/package name.
        old: Cpn,
        /// New category/package name.
        new: Cpn,
    },
    /// `slotmove <dep> <old_slot> <new_slot>` — slot renamed.
    SlotMove {
        /// Atom (possibly versioned) identifying affected packages.
        dep: Dep,
        /// Old slot value.
        old_slot: String,
        /// New slot value.
        new_slot: String,
    },
}

use crate::category::Category;
use crate::ebuild::Ebuild;
use crate::error::{Error, Result};
use crate::layout::LayoutConf;
use crate::profile::{Profile, ProfileDesc, ProfileStack};
use crate::shell::EbuildShell;
use crate::util;

/// A Gentoo ebuild repository.
///
/// This is the main entry point for the crate. It eagerly loads `layout.conf`
/// and the repository name, while category/package enumeration is lazy.
///
/// See [PMS 4 — Tree Layout](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone)]
pub struct Repository {
    path: PathBuf,
    layout: LayoutConf,
    name: String,
}

impl Repository {
    /// Open an ebuild repository at the given path.
    ///
    /// Reads `metadata/layout.conf` and `profiles/repo_name` eagerly.
    /// Returns an error if the directory lacks a valid `layout.conf`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.is_dir() {
            return Err(Error::InvalidRepository(path));
        }

        let layout = LayoutConf::from_repo(&path)?;

        let name = util::read_single_line(&path.join("profiles").join("repo_name"))?
            .unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });

        Ok(Repository { path, layout, name })
    }

    /// Absolute path to the repository root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Repository name (from `profiles/repo_name`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The parsed `metadata/layout.conf`.
    pub fn layout(&self) -> &LayoutConf {
        &self.layout
    }

    /// List all categories declared in `profiles/categories`.
    ///
    /// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn categories(&self) -> Result<Vec<Category>> {
        let lines = util::read_lines(&self.path.join("profiles").join("categories"))?;
        Ok(lines
            .into_iter()
            .map(|name| {
                let cat_path = self.path.join(&name);
                Category::new(name, cat_path)
            })
            .collect())
    }

    /// List all ebuilds in the repository using parallel directory walking.
    ///
    /// Uses [`jwalk`] to walk category directories concurrently, collecting
    /// all `.ebuild` files. Only categories listed in `profiles/categories`
    /// are visited. Results are sorted by CPV.
    ///
    /// See [PMS 4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn ebuilds(&self) -> Result<Vec<Ebuild>> {
        let categories: HashSet<String> =
            util::read_lines(&self.path.join("profiles").join("categories"))?
                .into_iter()
                .collect();

        let mut ebuilds: Vec<Ebuild> = WalkDir::new(&self.path)
            .min_depth(3)
            .max_depth(3)
            .process_read_dir(move |depth, _path, _state, children| {
                children.retain(|entry| {
                    entry.as_ref().is_ok_and(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        match depth {
                            // root entry itself — always keep
                            None => true,
                            // reading root dir → category dirs
                            Some(0) => categories.contains(name.as_ref()),
                            // reading category dir → package dirs
                            Some(1) => !name.starts_with('.'),
                            // reading package dir → ebuild files
                            _ => name.ends_with(".ebuild"),
                        }
                    })
                });
            })
            .into_iter()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                let stem = path.file_name()?.to_string_lossy();
                let stem = stem.strip_suffix(".ebuild")?;
                let cat_name = path.parent()?.parent()?.file_name()?.to_string_lossy();

                let cpv_str = format!("{cat_name}/{stem}");
                let cpv = Cpv::parse(&cpv_str).ok()?;
                Some(Ebuild::new(cpv, path))
            })
            .collect();

        ebuilds.sort_by(|a, b| a.cpv().cmp(b.cpv()));
        Ok(ebuilds)
    }

    /// Look up a single category by name.
    pub fn category(&self, name: &str) -> Option<Category> {
        let cat_path = self.path.join(name);
        if cat_path.is_dir() {
            Some(Category::new(name.to_string(), cat_path))
        } else {
            None
        }
    }

    /// Read a metadata cache entry for the given `Cpv`.
    ///
    /// Reads from `metadata/md5-cache/{category}/{package-version}`.
    ///
    /// See [PMS 14 — Metadata Cache](https://projects.gentoo.org/pms/9/pms.html#metadata-cache).
    pub fn cache_entry(&self, cpv: &Cpv) -> Result<CacheEntry> {
        let cache_path = self
            .path
            .join("metadata")
            .join("md5-cache")
            .join(cpv.to_string());
        let contents = util::read_to_string(&cache_path)?;
        Ok(CacheEntry::parse(&contents)?)
    }

    /// Parse `profiles/profiles.desc` to get available profile descriptions.
    ///
    /// See [PMS 5](https://projects.gentoo.org/pms/9/pms.html#profiles).
    pub fn profiles_desc(&self) -> Result<Vec<ProfileDesc>> {
        let lines = util::read_lines(&self.path.join("profiles").join("profiles.desc"))?;
        let mut descs = Vec::new();
        for line in lines {
            descs.push(ProfileDesc::parse(&line)?);
        }
        Ok(descs)
    }

    /// Read the default EAPI for profiles in this repository.
    ///
    /// Returns `None` if `profiles/eapi` is absent (EAPI 0 is implied).
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn profiles_eapi(&self) -> Result<Option<Eapi>> {
        match util::read_single_line(&self.path.join("profiles").join("eapi"))? {
            Some(s) => {
                let eapi = s.parse::<Eapi>().map_err(|e| {
                    Error::InvalidProfile(format!("bad EAPI in profiles/eapi: {e}"))
                })?;
                Ok(Some(eapi))
            }
            None => Ok(None),
        }
    }

    /// Parse the repository-level `profiles/package.mask`.
    ///
    /// These masks apply across all profiles in the repository and should
    /// be merged before any profile-stack masks.  Returns an empty `Vec`
    /// if the file is absent.
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn repo_package_mask(&self) -> Result<Vec<Dep>> {
        let lines = util::read_lines(&self.path.join("profiles").join("package.mask"))?;
        lines
            .into_iter()
            .map(|l| Dep::parse(&l).map_err(Into::into))
            .collect()
    }

    /// List available USE_EXPAND variable names from `profiles/desc/`.
    ///
    /// Returns the stem of each `.desc` file (e.g. `"cpu_flags_x86"`),
    /// sorted alphabetically.
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn use_expand_names(&self) -> Result<Vec<String>> {
        let dir = self.path.join("profiles").join("desc");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&dir, e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| util::io_err(&dir, e))?;
            let fname = entry.file_name();
            let fname = fname.to_string_lossy();
            if let Some(stem) = fname.strip_suffix(".desc")
                && !stem.starts_with('.')
            {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Parse USE_EXPAND flag descriptions from `profiles/desc/{name}.desc`.
    ///
    /// Returns `(flag_name, description)` pairs.  Returns an empty `Vec`
    /// if the file does not exist.
    ///
    /// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
    pub fn use_expand_desc(&self, name: &str) -> Result<Vec<(String, String)>> {
        parse_desc_file(
            &self
                .path
                .join("profiles")
                .join("desc")
                .join(format!("{name}.desc")),
        )
    }

    /// Parse all package-move and slot-move entries from `profiles/updates/`.
    ///
    /// Files are read in sorted order (oldest first by filename convention).
    /// Lines with unrecognised tags or parse errors are silently skipped.
    ///
    /// See [PMS 4.4.4](https://projects.gentoo.org/pms/9/pms.html#profiles-updates).
    pub fn profile_updates(&self) -> Result<Vec<ProfileUpdate>> {
        let dir = self.path.join("profiles").join("updates");
        let mut files: Vec<_> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| e.path())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&dir, e)),
        };
        files.sort();

        let mut updates = Vec::new();
        for file in files {
            for line in util::read_lines(&file)? {
                let mut parts = line.split_whitespace();
                match parts.next() {
                    Some("move") => {
                        let (Some(old_s), Some(new_s)) = (parts.next(), parts.next()) else {
                            continue;
                        };
                        let (Ok(old), Ok(new)) = (Cpn::parse(old_s), Cpn::parse(new_s)) else {
                            continue;
                        };
                        updates.push(ProfileUpdate::Move { old, new });
                    }
                    Some("slotmove") => {
                        let (Some(dep_s), Some(old_s), Some(new_s)) =
                            (parts.next(), parts.next(), parts.next())
                        else {
                            continue;
                        };
                        let Ok(dep) = Dep::parse(dep_s) else { continue };
                        updates.push(ProfileUpdate::SlotMove {
                            dep,
                            old_slot: old_s.to_string(),
                            new_slot: new_s.to_string(),
                        });
                    }
                    _ => continue, // unknown tag — skip
                }
            }
        }
        Ok(updates)
    }

    /// Open a profile directory relative to `profiles/`.
    pub fn profile(&self, relative_path: &str) -> Result<Profile> {
        let profile_path = self.path.join("profiles").join(relative_path);
        Profile::open(profile_path)
    }

    /// Build the full profile stack for a profile relative to `profiles/`.
    ///
    /// Follows `parent` files recursively and returns a [`ProfileStack`] with
    /// all ancestor profiles in resolution order.
    ///
    /// See [PMS 5.1](https://projects.gentoo.org/pms/9/pms.html#profiles).
    pub fn profile_stack(&self, relative_path: &str) -> Result<ProfileStack> {
        let profile_path = self.path.join("profiles").join(relative_path);
        ProfileStack::build(profile_path)
    }

    /// List available eclass names (without the `.eclass` extension).
    pub fn eclasses(&self) -> Result<Vec<String>> {
        let eclass_dir = self.path.join("eclass");
        let entries = match std::fs::read_dir(&eclass_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(util::io_err(&eclass_dir, e)),
        };

        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| util::io_err(&eclass_dir, e))?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".eclass") {
                names.push(stem.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// List available license names from `licenses/`.
    pub fn licenses(&self) -> Result<Vec<String>> {
        list_dir_names(&self.path.join("licenses"))
    }

    /// List architecture keywords from `profiles/arch.list`.
    pub fn arch_list(&self) -> Result<Vec<String>> {
        util::read_lines(&self.path.join("profiles").join("arch.list"))
    }

    /// Parse global USE flag descriptions from `profiles/use.desc`.
    ///
    /// Returns `(flag_name, description)` pairs.
    pub fn use_desc(&self) -> Result<Vec<(String, String)>> {
        parse_desc_file(&self.path.join("profiles").join("use.desc"))
    }

    /// Parse per-package USE flag descriptions from `profiles/use.local.desc`.
    ///
    /// Returns `(Cpn, flag_name, description)` tuples.
    pub fn use_local_desc(&self) -> Result<Vec<(Cpn, String, String)>> {
        let lines = util::read_lines(&self.path.join("profiles").join("use.local.desc"))?;
        let mut result = Vec::new();
        for line in lines {
            // Format: category/package:flag - description
            let Some((cpn_str, rest)) = line.split_once(':') else {
                continue;
            };
            let cpn = Cpn::parse(cpn_str)?;
            let (flag, desc) = if let Some((f, d)) = rest.split_once(" - ") {
                (f.to_string(), d.to_string())
            } else {
                (rest.to_string(), String::new())
            };
            result.push((cpn, flag, desc));
        }
        Ok(result)
    }

    /// Parse `profiles/thirdpartymirrors`.
    ///
    /// Returns `(mirror_name, [urls...])` pairs.
    pub fn thirdpartymirrors(&self) -> Result<Vec<(String, Vec<String>)>> {
        let lines = util::read_lines(&self.path.join("profiles").join("thirdpartymirrors"))?;
        let mut result = Vec::new();
        for line in lines {
            let mut parts = line.split_whitespace();
            if let Some(name) = parts.next() {
                let urls: Vec<String> = parts.map(String::from).collect();
                result.push((name.to_string(), urls));
            }
        }
        Ok(result)
    }

    /// Create an [`EbuildShell`] configured for this repository.
    ///
    /// The shell will have eclass directories set up based on the repository
    /// layout (this repo's `eclass/` directory).
    pub async fn shell(&self) -> Result<EbuildShell> {
        EbuildShell::new(self).await
    }

    /// Create an [`EbuildShell`] with master repository eclass directories.
    ///
    /// Master eclass directories are prepended (searched first), matching
    /// Portage's resolution order. The overlay's own `eclass/` directory
    /// is searched last.
    ///
    /// See [PMS 4.7](https://projects.gentoo.org/pms/9/pms.html#tree-layout)
    /// and [PMS 10.1](https://projects.gentoo.org/pms/9/pms.html#eclasses).
    pub async fn shell_with_masters(&self, masters: &[&Repository]) -> Result<EbuildShell> {
        let mut shell = EbuildShell::new(self).await?;
        // Prepend master eclass dirs in reverse order so the first master
        // ends up at position 0 (highest priority among masters).
        for master in masters.iter().rev() {
            let dir = master.path().join("eclass");
            if dir.is_dir() {
                shell.prepend_eclass_dir(dir);
            }
        }
        Ok(shell)
    }

    /// Create an [`EbuildShell`] with a profile's USE configuration applied.
    ///
    /// `profile_rel_path` is relative to the repository's `profiles/` directory,
    /// e.g. `"default/linux/amd64/17.1"`.
    ///
    /// `make_conf` is an optional path to a `make.conf`-style shell script
    /// (typically `/etc/portage/make.conf`). When provided it is sourced after
    /// the profile `make.defaults` chain but before `use.force`/`use.mask`,
    /// matching Portage's USE flag precedence order.
    ///
    /// To also include master repository eclasses, create the shell with
    /// [`Repository::shell_with_masters`] and then call [`ProfileStack::configure_shell`]
    /// manually.
    ///
    /// See [PMS 5.2](https://projects.gentoo.org/pms/9/pms.html#profiles).
    pub async fn shell_with_profile(
        &self,
        profile_rel_path: &str,
        make_conf: Option<&std::path::Path>,
    ) -> Result<EbuildShell> {
        let path = self.path.join("profiles").join(profile_rel_path);
        let stack = ProfileStack::build(path)?;
        let mut shell = EbuildShell::new(self).await?;
        let confs: Vec<&std::path::Path> = make_conf.into_iter().collect();
        stack.configure_shell(&mut shell, &confs).await?;
        Ok(shell)
    }

    /// Open a repository, resolving its master repositories from `repos_dir`.
    ///
    /// Each master listed in `layout.conf` is opened from
    /// `repos_dir/<master_name>`, and its own masters are resolved
    /// recursively (depth-first). Returns the opened repository and
    /// the flattened list of master repositories in search order.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use portage_repo::Repository;
    ///
    /// let (overlay, masters) = Repository::open_with_masters(
    ///     "/var/db/repos/my-overlay",
    ///     "/var/db/repos",
    /// ).unwrap();
    /// ```
    pub fn open_with_masters(
        path: impl Into<PathBuf>,
        repos_dir: impl AsRef<Path>,
    ) -> Result<(Self, Vec<Repository>)> {
        let repo = Self::open(path)?;
        let mut masters = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(repo.name().to_string());
        Self::resolve_masters(&repo, repos_dir.as_ref(), &mut masters, &mut seen)?;
        Ok((repo, masters))
    }

    /// Recursively resolve master repositories (depth-first).
    fn resolve_masters(
        repo: &Repository,
        repos_dir: &Path,
        out: &mut Vec<Repository>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        for master_name in &repo.layout().masters {
            if !seen.insert(master_name.clone()) {
                continue; // already resolved or cycle
            }
            let master_path = repos_dir.join(master_name);
            let master = Self::open(master_path)?;
            // Resolve the master's own masters first (depth-first).
            Self::resolve_masters(&master, repos_dir, out, seen)?;
            out.push(master);
        }
        Ok(())
    }
}

/// List file/directory names in a directory (sorted, skipping dotfiles).
fn list_dir_names(dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(util::io_err(dir, e)),
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| util::io_err(dir, e))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.starts_with('.') && name != "CVS" {
            names.push(name.into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Parse a `flag - description` file format used by `use.desc` etc.
fn parse_desc_file(path: &Path) -> Result<Vec<(String, String)>> {
    let lines = util::read_lines(path)?;
    let mut result = Vec::new();
    for line in lines {
        if let Some((flag, desc)) = line.split_once(" - ") {
            result.push((flag.to_string(), desc.to_string()));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create the minimal directory structure required by `Repository::open`.
    fn make_test_repo(dir: &tempfile::TempDir) -> Repository {
        std::fs::create_dir_all(dir.path().join("metadata")).unwrap();
        std::fs::write(dir.path().join("metadata").join("layout.conf"), "").unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        Repository::open(dir.path()).unwrap()
    }

    #[test]
    fn profiles_eapi_absent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.profiles_eapi().unwrap().is_none());
    }

    #[test]
    fn profiles_eapi_returns_parsed_eapi() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(dir.path().join("profiles").join("eapi"), "5\n").unwrap();
        assert_eq!(repo.profiles_eapi().unwrap(), Some(Eapi::Five));
    }

    #[test]
    fn repo_package_mask_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.repo_package_mask().unwrap().is_empty());
    }

    #[test]
    fn repo_package_mask_parses_atoms() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        std::fs::write(
            dir.path().join("profiles").join("package.mask"),
            "# comment\ndev-libs/foo\ndev-libs/bar\n",
        )
        .unwrap();
        let masks = repo.repo_package_mask().unwrap();
        assert_eq!(masks.len(), 2);
    }

    #[test]
    fn use_expand_names_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.use_expand_names().unwrap().is_empty());
    }

    #[test]
    fn use_expand_names_and_desc() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let desc_dir = dir.path().join("profiles").join("desc");
        std::fs::create_dir_all(&desc_dir).unwrap();
        std::fs::write(
            desc_dir.join("cpu_flags_x86.desc"),
            "mmx - MMX instruction support\nsse2 - SSE2 support\n",
        )
        .unwrap();

        let names = repo.use_expand_names().unwrap();
        assert_eq!(names, vec!["cpu_flags_x86"]);

        let descs = repo.use_expand_desc("cpu_flags_x86").unwrap();
        assert_eq!(descs.len(), 2);
        assert_eq!(
            descs[0],
            ("mmx".to_string(), "MMX instruction support".to_string())
        );
        assert_eq!(descs[1], ("sse2".to_string(), "SSE2 support".to_string()));
    }

    #[test]
    fn use_expand_desc_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.use_expand_desc("nonexistent").unwrap().is_empty());
    }

    #[test]
    fn profile_updates_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        assert!(repo.profile_updates().unwrap().is_empty());
    }

    #[test]
    fn profile_updates_parses_move_and_slotmove() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let updates_dir = dir.path().join("profiles").join("updates");
        std::fs::create_dir_all(&updates_dir).unwrap();
        std::fs::write(
            updates_dir.join("1Q-2024"),
            "# comment\nmove dev-libs/foo dev-libs/bar\nslotmove >=dev-libs/baz-1.0 0 1\n",
        )
        .unwrap();

        let updates = repo.profile_updates().unwrap();
        assert_eq!(updates.len(), 2);
        assert!(matches!(&updates[0], ProfileUpdate::Move { old, new }
            if old.to_string() == "dev-libs/foo" && new.to_string() == "dev-libs/bar"));
        assert!(
            matches!(&updates[1], ProfileUpdate::SlotMove { old_slot, new_slot, .. }
            if old_slot == "0" && new_slot == "1")
        );
    }

    #[test]
    fn profile_updates_skips_unknown_tags() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_test_repo(&dir);
        let updates_dir = dir.path().join("profiles").join("updates");
        std::fs::create_dir_all(&updates_dir).unwrap();
        std::fs::write(
            updates_dir.join("1Q-2024"),
            "unknown_tag foo bar\nmove dev-libs/a dev-libs/b\n",
        )
        .unwrap();

        let updates = repo.profile_updates().unwrap();
        assert_eq!(updates.len(), 1);
        assert!(matches!(&updates[0], ProfileUpdate::Move { .. }));
    }
}
