use std::collections::BTreeMap;

/// A set of USE_EXPAND group names that can bucket USE flag lists.
///
/// USE_EXPAND groups (e.g. `CPU_FLAGS_X86`, `VIDEO_CARDS`) are stored as
/// lowercase prefixes sorted longest-first so that a longer prefix like
/// `cpu_flags_x86` is always matched before a hypothetical shorter prefix
/// `cpu_flags`.
///
/// # Example
///
/// ```
/// use portage_repo::UseExpand;
///
/// let expand = UseExpand::new(["cpu_flags_x86", "video_cards"]);
/// let groups = expand.group(["cpu_flags_x86_sse2", "video_cards_intel", "wayland"]);
/// assert_eq!(groups["cpu_flags_x86"], ["sse2"]);
/// assert_eq!(groups["video_cards"], ["intel"]);
/// assert_eq!(groups["global"], ["wayland"]);
/// ```
///
/// See [PMS 4.4](https://projects.gentoo.org/pms/9/pms.html#tree-layout).
#[derive(Debug, Clone, Default)]
pub struct UseExpand {
    /// Lowercase group names sorted longest-first for unambiguous prefix matching.
    prefixes: Vec<String>,
}

impl UseExpand {
    /// Create from an iterator of group names (case-insensitive).
    ///
    /// Accepts the output of `Repository::use_expand_names` directly.
    pub fn new(group_names: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut prefixes: Vec<String> = group_names
            .into_iter()
            .map(|n| n.as_ref().to_lowercase())
            .collect();
        // Longest-first so a longer prefix always wins over a shorter one.
        prefixes.sort_by_key(|b| std::cmp::Reverse(b.len()));
        Self { prefixes }
    }

    /// Parse from a space-separated `$USE_EXPAND` variable value.
    ///
    /// This is the variable set by `make.defaults` in the profile stack.
    /// After `ProfileStack::configure_shell` you can read it with
    /// `shell.get_var("USE_EXPAND")`.
    pub fn from_var(use_expand: &str) -> Self {
        Self::new(use_expand.split_whitespace())
    }

    /// Group USE flags by their USE_EXPAND prefix.
    ///
    /// Each flag is matched against the known prefixes (longest first).  On a
    /// match the prefix and the separating `_` are stripped, leaving the value
    /// (e.g. `cpu_flags_x86_sse2` → group `cpu_flags_x86`, value `sse2`).
    /// Flags with no matching prefix are placed in the `"global"` group.
    ///
    /// Values within each group are **not** sorted — the caller decides order.
    pub fn group(
        &self,
        flags: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> BTreeMap<String, Vec<String>> {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for flag in flags {
            let flag = flag.as_ref();
            let (bucket, value) = self.split(flag);
            groups.entry(bucket.to_string()).or_default().push(value);
        }
        groups
    }

    /// The known lowercase group prefixes, sorted longest-first.
    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    /// Split one flag into `(group, value)`.
    ///
    /// Returns `("global", flag.to_string())` if no prefix matches.
    pub fn split(&self, flag: &str) -> (&str, String) {
        for prefix in &self.prefixes {
            if let Some(rest) = flag.strip_prefix(prefix.as_str())
                && let Some(value) = rest.strip_prefix('_')
            {
                return (prefix.as_str(), value.to_string());
            }
        }
        ("global", flag.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_flags_by_prefix() {
        let expand = UseExpand::new(["cpu_flags_x86", "video_cards"]);
        let groups =
            expand.group(["cpu_flags_x86_sse2", "video_cards_intel", "wayland"]);
        assert_eq!(groups["cpu_flags_x86"], ["sse2"]);
        assert_eq!(groups["video_cards"], ["intel"]);
        assert_eq!(groups["global"], ["wayland"]);
    }

    #[test]
    fn longest_prefix_wins() {
        // "cpu_flags_x86" must match before "cpu_flags" if both were present.
        let expand = UseExpand::new(["cpu_flags", "cpu_flags_x86"]);
        let (group, value) = expand.split("cpu_flags_x86_sse2");
        assert_eq!(group, "cpu_flags_x86");
        assert_eq!(value, "sse2");
    }

    #[test]
    fn from_var_parses_space_separated() {
        let expand = UseExpand::from_var("CPU_FLAGS_X86 VIDEO_CARDS  ELIBC");
        let prefixes: std::collections::HashSet<_> = expand.prefixes().iter().collect();
        assert!(prefixes.contains(&"cpu_flags_x86".to_string()));
        assert!(prefixes.contains(&"video_cards".to_string()));
        assert!(prefixes.contains(&"elibc".to_string()));
        assert_eq!(expand.prefixes().len(), 3);
    }

    #[test]
    fn global_fallback() {
        let expand = UseExpand::new(["video_cards"]);
        let (group, value) = expand.split("wayland");
        assert_eq!(group, "global");
        assert_eq!(value, "wayland");
    }

    #[test]
    fn empty_flags_returns_empty_map() {
        let expand = UseExpand::new(["video_cards"]);
        assert!(expand.group([] as [&str; 0]).is_empty());
    }
}
