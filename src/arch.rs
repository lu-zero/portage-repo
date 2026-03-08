//! Typed Gentoo architecture token with lasso string interning.
//!
//! Known architectures delegate to [`gentoo_core::Arch`] (zero-cost, `Copy`).
//! Overlay-defined (exotic) architectures are interned in the owning
//! repository's interner; `lasso` is an implementation detail of this crate.

use gentoo_core::Arch as KnownArch;
use lasso::ThreadedRodeo;

/// Opaque token for an overlay-defined architecture keyword.
///
/// The inner `lasso::Spur` is private; values can only be created inside this
/// crate. Resolution back to a string is done via
/// [`Repository::arch_keyword`](crate::Repository::arch_keyword), which is
/// infallible because every `ExoticKey` is interned in the same
/// [`ThreadedRodeo`] that the owning repository holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExoticKey(lasso::Spur);

/// A Gentoo architecture token scoped to a repository's arch list.
///
/// `Known` maps to `gentoo_core::Arch` (zero-cost, `Copy`).
/// `Exotic` holds an [`ExoticKey`] that is only meaningful when resolved
/// through the [`Repository`](crate::Repository) that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    /// A standard Gentoo architecture keyword.
    Known(KnownArch),
    /// An overlay-defined architecture keyword.
    Exotic(ExoticKey),
}

impl Arch {
    /// Parse `keyword` against known arches; intern anything unknown.
    pub(crate) fn intern(keyword: &str, interner: &ThreadedRodeo) -> Self {
        if let Ok(known) = KnownArch::parse(keyword) {
            Self::Known(known)
        } else {
            Self::Exotic(ExoticKey(interner.get_or_intern(keyword)))
        }
    }

    /// Extract the CPU part of a GNU CHOST triple and return an `Arch`.
    ///
    /// Returns `None` only when `chost` is empty.
    pub(crate) fn from_chost(chost: &str, interner: &ThreadedRodeo) -> Option<Self> {
        let cpu = chost.split('-').next().filter(|s| !s.is_empty())?;
        Some(Self::intern(&normalize_chost_cpu(cpu), interner))
    }

    /// Resolve to the Gentoo keyword string.
    ///
    /// Infallible: every `ExoticKey` was interned in the same `ThreadedRodeo`
    /// that `Repository` passes here, so `resolve` will always find the key.
    pub(crate) fn as_keyword<'a>(&self, interner: &'a ThreadedRodeo) -> &'a str {
        match self {
            Self::Known(arch) => arch.as_keyword(),
            Self::Exotic(ExoticKey(spur)) => interner.resolve(spur),
        }
    }
}

/// Normalise the CPU field of a GNU CHOST triple before matching known arches.
fn normalize_chost_cpu(cpu: &str) -> String {
    let s = cpu.to_lowercase();

    // powerpc64le / powerpc64be → powerpc64
    for suffix in &["le", "be"] {
        if let Some(base) = s.strip_suffix(suffix)
            && base == "powerpc64"
        {
            return base.to_string();
        }
    }

    // mipsel / mipseb → mips;  mips64el / mips64eb → mips64
    for suffix in &["el", "eb"] {
        if let Some(base) = s.strip_suffix(suffix)
            && (base == "mips" || base == "mips64")
        {
            return base.to_string();
        }
    }

    // riscv64gc, riscv64imac → riscv64;  riscv32gc → riscv32
    if let Some(after_riscv) = s.strip_prefix("riscv") {
        if let Some(end) = after_riscv.find(|c: char| !c.is_ascii_digit())
            && end > 0
        {
            return format!("riscv{}", &after_riscv[..end]);
        }
        return s;
    }

    // hppa2.0w, hppa1.1 → hppa
    if s.starts_with("hppa") && s.len() > "hppa".len() {
        return "hppa".to_string();
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_interner() -> ThreadedRodeo {
        ThreadedRodeo::default()
    }

    #[test]
    fn known_arches_intern() {
        let i = make_interner();
        assert!(matches!(Arch::intern("amd64", &i), Arch::Known(_)));
        assert!(matches!(Arch::intern("arm64", &i), Arch::Known(_)));
        assert!(matches!(Arch::intern("loong", &i), Arch::Known(_)));
        assert!(matches!(Arch::intern("hppa", &i), Arch::Known(_)));
    }

    #[test]
    fn exotic_arches_intern() {
        let i = make_interner();
        let a1 = Arch::intern("mymachine", &i);
        assert!(matches!(a1, Arch::Exotic(_)));
        // Same key on second intern
        let a2 = Arch::intern("mymachine", &i);
        assert_eq!(a1, a2);
        // Resolves correctly
        assert_eq!(a1.as_keyword(&i), "mymachine");
    }

    #[test]
    fn chost_known() {
        let i = make_interner();
        let cases = [
            ("x86_64-pc-linux-gnu", "amd64"),
            ("aarch64-unknown-linux-gnu", "arm64"),
            ("i686-pc-linux-gnu", "x86"),
            ("powerpc-unknown-linux-gnu", "ppc"),
            ("s390x-linux-gnu", "s390"),
        ];
        for (chost, expected) in cases {
            let arch = Arch::from_chost(chost, &i).unwrap();
            assert_eq!(arch.as_keyword(&i), expected, "chost={chost}");
            assert!(matches!(arch, Arch::Known(_)), "chost={chost} should be Known");
        }
    }

    #[test]
    fn chost_normalization() {
        let i = make_interner();
        let cases = [
            ("powerpc64le-unknown-linux-gnu", "ppc64"),
            ("riscv64gc-unknown-linux-gnu", "riscv"),
            ("mipsel-unknown-linux-gnu", "mips"),
            ("mips64el-unknown-linux-gnu", "mips"),
            ("hppa2.0w-hp-linux-gnu", "hppa"),
        ];
        for (chost, expected_keyword) in cases {
            let arch = Arch::from_chost(chost, &i).unwrap();
            assert_eq!(arch.as_keyword(&i), expected_keyword, "chost={chost}");
        }
    }

    #[test]
    fn empty_chost() {
        let i = make_interner();
        assert!(Arch::from_chost("", &i).is_none());
    }
}
