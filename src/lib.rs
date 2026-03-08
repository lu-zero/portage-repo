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
mod inherit;
mod layout;
mod manifest;
mod package;
mod pkgmetadata;
mod pms_builtins;
mod profile;
mod repository;
mod shell;
mod use_expand;
mod util;
mod ver_funcs;

pub use category::Category;
pub use ebuild::Ebuild;
pub use error::{Error, Result};
pub use gentoo_core::{
    Arch, DefaultInterner, ExoticKey, GlobalInterner, Interner, KnownArch, NoInterner,
};
pub use layout::LayoutConf;
pub use manifest::{Manifest, ManifestEntry};
pub use package::Package;
pub use pkgmetadata::PkgMetadata;
pub use profile::{Profile, ProfileDesc, ProfileStack, ProfileStatus};
pub use repository::{ProfileUpdate, Repository};
pub use shell::EbuildShell;
pub use use_expand::UseExpand;
