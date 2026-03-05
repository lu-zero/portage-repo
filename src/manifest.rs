use crate::error::{Error, Result};

/// A single entry in a `Manifest` file (GLEP 74).
///
/// See [GLEP 74](https://www.gentoo.org/glep/glep-0074.html).
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestEntry {
    /// `DIST` — a distfile (downloaded source archive).
    Dist {
        filename: String,
        size: u64,
        hashes: Vec<(String, String)>,
    },
    /// `DATA` / `EBUILD` / `MISC` / `AUX` — a tracked repo file.
    Data {
        path: String,
        size: u64,
        hashes: Vec<(String, String)>,
    },
    /// `MANIFEST` — a sub-manifest reference.
    SubManifest {
        path: String,
        size: u64,
        hashes: Vec<(String, String)>,
    },
    /// `IGNORE` — path excluded from manifest checks.
    Ignore { path: String },
    /// `TIMESTAMP` — last-updated RFC 3339 timestamp.
    Timestamp { value: String },
}

/// Parsed representation of a `Manifest` file (GLEP 74).
///
/// In practice, the Gentoo repository uses `thin-manifests = true`, so only
/// `DIST` entries appear.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// Parse a `Manifest` from its text content.
    ///
    /// Blank lines and lines starting with `#` are silently skipped.
    /// Unknown type tags are also silently skipped for forward compatibility.
    pub fn parse(input: &str) -> Result<Self> {
        let mut entries = Vec::new();

        for (lineno, raw) in input.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let mut tokens = line.split_ascii_whitespace();
            let tag = match tokens.next() {
                Some(t) => t,
                None => continue,
            };

            let entry = match tag {
                "DIST" => {
                    let (path, size, hashes) = parse_path_size_hashes(tag, line, lineno)?;
                    ManifestEntry::Dist { filename: path, size, hashes }
                }
                "DATA" | "EBUILD" | "MISC" | "AUX" => {
                    let (path, size, hashes) = parse_path_size_hashes(tag, line, lineno)?;
                    ManifestEntry::Data { path, size, hashes }
                }
                "MANIFEST" => {
                    let (path, size, hashes) = parse_path_size_hashes(tag, line, lineno)?;
                    ManifestEntry::SubManifest { path, size, hashes }
                }
                "IGNORE" => {
                    let path = tokens.next().ok_or_else(|| {
                        Error::InvalidManifest(format!(
                            "line {}: IGNORE requires a path",
                            lineno + 1
                        ))
                    })?;
                    ManifestEntry::Ignore { path: path.to_string() }
                }
                "TIMESTAMP" => {
                    let value = tokens.next().ok_or_else(|| {
                        Error::InvalidManifest(format!(
                            "line {}: TIMESTAMP requires a value",
                            lineno + 1
                        ))
                    })?;
                    ManifestEntry::Timestamp { value: value.to_string() }
                }
                _ => {
                    // Unknown type tag — skip for forward compatibility.
                    continue;
                }
            };

            entries.push(entry);
        }

        Ok(Manifest { entries })
    }

    /// Iterate over `DIST` entries only.
    pub fn dist_entries(&self) -> impl Iterator<Item = &ManifestEntry> {
        self.entries.iter().filter(|e| matches!(e, ManifestEntry::Dist { .. }))
    }
}

/// Parse `tag path size [algo hex ...]` from a whitespace-split line.
fn parse_path_size_hashes(
    tag: &str,
    line: &str,
    lineno: usize,
) -> Result<(String, u64, Vec<(String, String)>)> {
    let mut tokens = line.split_ascii_whitespace();
    tokens.next(); // skip the tag itself

    let path = tokens.next().ok_or_else(|| {
        Error::InvalidManifest(format!("line {}: {} requires a path", lineno + 1, tag))
    })?;

    let size_str = tokens.next().ok_or_else(|| {
        Error::InvalidManifest(format!("line {}: {} requires a size", lineno + 1, tag))
    })?;

    let size: u64 = size_str.parse().map_err(|_| {
        Error::InvalidManifest(format!(
            "line {}: {} size {:?} is not a valid integer",
            lineno + 1,
            tag,
            size_str
        ))
    })?;

    let mut hashes = Vec::new();
    loop {
        match (tokens.next(), tokens.next()) {
            (Some(algo), Some(hex)) => hashes.push((algo.to_string(), hex.to_string())),
            (None, _) => break,
            (Some(algo), None) => {
                return Err(Error::InvalidManifest(format!(
                    "line {}: hash algo {:?} has no corresponding hex value",
                    lineno + 1,
                    algo
                )));
            }
        }
    }

    Ok((path.to_string(), size, hashes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dist_entry() {
        let input = "DIST foo-1.0.tar.gz 12345 SHA256 abcd1234 SHA512 ef567890\n";
        let manifest = Manifest::parse(input).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        match &manifest.entries[0] {
            ManifestEntry::Dist { filename, size, hashes } => {
                assert_eq!(filename, "foo-1.0.tar.gz");
                assert_eq!(*size, 12345);
                assert_eq!(hashes, &[("SHA256".into(), "abcd1234".into()), ("SHA512".into(), "ef567890".into())]);
            }
            _ => panic!("expected Dist"),
        }
    }

    #[test]
    fn parse_ignore_entry() {
        let input = "IGNORE some/path\n";
        let manifest = Manifest::parse(input).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        match &manifest.entries[0] {
            ManifestEntry::Ignore { path } => assert_eq!(path, "some/path"),
            _ => panic!("expected Ignore"),
        }
    }

    #[test]
    fn parse_timestamp_entry() {
        let input = "TIMESTAMP 2024-01-15T12:00:00Z\n";
        let manifest = Manifest::parse(input).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        match &manifest.entries[0] {
            ManifestEntry::Timestamp { value } => assert_eq!(value, "2024-01-15T12:00:00Z"),
            _ => panic!("expected Timestamp"),
        }
    }

    #[test]
    fn parse_data_aliases() {
        for tag in &["DATA", "EBUILD", "MISC", "AUX"] {
            let input = format!("{} files/foo.patch 42 SHA256 deadbeef\n", tag);
            let manifest = Manifest::parse(&input).unwrap();
            assert_eq!(manifest.entries.len(), 1);
            assert!(matches!(manifest.entries[0], ManifestEntry::Data { .. }));
        }
    }

    #[test]
    fn parse_sub_manifest() {
        let input = "MANIFEST sub/Manifest 99 SHA256 cafebabe\n";
        let manifest = Manifest::parse(input).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        assert!(matches!(manifest.entries[0], ManifestEntry::SubManifest { .. }));
    }

    #[test]
    fn parse_empty_file() {
        let manifest = Manifest::parse("").unwrap();
        assert!(manifest.entries.is_empty());
    }

    #[test]
    fn parse_blank_lines_and_comments() {
        let input = "# This is a comment\n\nDIST bar-2.0.tar.xz 99 SHA256 ff00\n\n# end\n";
        let manifest = Manifest::parse(input).unwrap();
        assert_eq!(manifest.entries.len(), 1);
    }

    #[test]
    fn parse_unknown_type_skipped() {
        let input = "FUTURE somepath 0 SHA256 aabbcc\nDIST foo.tar.gz 1 SHA256 001122\n";
        let manifest = Manifest::parse(input).unwrap();
        assert_eq!(manifest.entries.len(), 1);
        assert!(matches!(manifest.entries[0], ManifestEntry::Dist { .. }));
    }

    #[test]
    fn parse_multiple_entries() {
        let input = "\
DIST a.tar.gz 100 SHA256 aa
DIST b.tar.bz2 200 SHA256 bb
IGNORE obsolete/
TIMESTAMP 2024-06-01T00:00:00Z
";
        let manifest = Manifest::parse(input).unwrap();
        assert_eq!(manifest.entries.len(), 4);
        assert_eq!(manifest.dist_entries().count(), 2);
    }

    #[test]
    fn parse_dist_no_hashes() {
        // DIST with just path and size, no hash pairs — valid edge case.
        let input = "DIST foo.tar.gz 42\n";
        let manifest = Manifest::parse(input).unwrap();
        match &manifest.entries[0] {
            ManifestEntry::Dist { hashes, .. } => assert!(hashes.is_empty()),
            _ => panic!("expected Dist"),
        }
    }

    #[test]
    fn parse_error_dist_missing_size() {
        let input = "DIST foo.tar.gz\n";
        assert!(Manifest::parse(input).is_err());
    }

    #[test]
    fn parse_error_dist_bad_size() {
        let input = "DIST foo.tar.gz notanumber SHA256 aabb\n";
        assert!(Manifest::parse(input).is_err());
    }

    #[test]
    fn parse_error_orphan_hash_algo() {
        let input = "DIST foo.tar.gz 42 SHA256\n";
        assert!(Manifest::parse(input).is_err());
    }
}
