//! Gentoo ebuild repository layout reader based on the
//! [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/9/pms.html).
//!
//! This crate provides types for reading and navigating a Gentoo ebuild
//! repository: `metadata/layout.conf`, category and package directory
//! enumeration, profiles, metadata cache access, and ebuild/eclass sourcing
//! via an embedded bash shell ([brush](https://crates.io/crates/brush-core)).
//!
//! # Quick start
//!
//! ```no_run
//! use portage_repo::Repository;
//!
//! let repo = Repository::open("/var/db/repos/gentoo").unwrap();
//! println!("repo: {} (masters: {:?})", repo.name(), repo.layout().masters);
//!
//! for cat in repo.categories().unwrap() {
//!     for pkg in cat.packages().unwrap() {
//!         for ebuild in pkg.ebuilds().unwrap() {
//!             println!("{}", ebuild.cpv());
//!         }
//!     }
//! }
//! ```
//!
//! # Crate family
//!
//! - [`portage-atom`](https://crates.io/crates/portage-atom) — PMS atom parser
//! - [`portage-metadata`](https://crates.io/crates/portage-metadata) — metadata cache types
//! - `portage-repo` (this crate) — repository layout reader
//!
//! > **Warning**: This codebase was largely AI-generated and has not yet been
//! > thoroughly audited. It may contain bugs, incomplete PMS coverage, or
//! > surprising edge-case behaviour. Use at your own risk.

mod builtins;
mod category;
mod ebuild;
mod error;
mod layout;
mod package;
mod profile;
mod repository;
mod shell;
mod util;

pub use category::Category;
pub use ebuild::Ebuild;
pub use error::{Error, Result};
pub use layout::LayoutConf;
pub use package::Package;
pub use profile::{Profile, ProfileDesc, ProfileStatus};
pub use repository::Repository;
pub use shell::EbuildShell;
