use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jwalk::WalkDir;
use portage_atom::{Cpn, Cpv};
use portage_metadata::CacheEntry;

use crate::category::Category;
use crate::ebuild::Ebuild;
use crate::error::{Error, Result};
use crate::layout::LayoutConf;
use crate::profile::{Profile, ProfileDesc};
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
        let categories: HashSet<String> = util::read_lines(
            &self.path.join("profiles").join("categories"),
        )?
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

    /// Open a profile directory relative to `profiles/`.
    pub fn profile(&self, relative_path: &str) -> Result<Profile> {
        let profile_path = self.path.join("profiles").join(relative_path);
        Profile::open(profile_path)
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
