# portage-repo Status

## PMS 9 Compliance

Target specification: [PMS 9](https://projects.gentoo.org/pms/9/pms.html)

### What works

#### Repository layout (PMS 4)
- `Repository::open()` — reads `layout.conf` and `profiles/repo_name`
- Category / package / ebuild enumeration via `categories()`, `packages()`, `ebuilds()`
- Metadata cache reading via `cache_entry(&cpv)` (`metadata/md5-cache/`)
- Profile descriptions (`profiles.desc`), USE flag descriptions, arch list, mirrors
- Eclass and license listing
- Dotfiles skipped in category/package enumeration

#### Profiles (PMS 5)
- `parent`, `eapi`, `packages`, `package.mask`, `package.use`
- `use.force`, `use.mask`, `use.stable.force`, `use.stable.mask`
- `package.use.force`, `package.use.mask`, `package.use.stable.force`,
  `package.use.stable.mask`
- `make.defaults` sourced through embedded bash shell

#### Embedded bash shell (PMS 10, 12)
- Full embedded shell via brush-core with the winnow parser
- Eclass sourcing via `inherit()` with `INHERITED` tracking and `ECLASS` scoping
- `EXPORT_FUNCTIONS` phase alias creation
- PM-provided variables: `CATEGORY`, `PN`, `PV`, `PVR`, `P`, `PF`, `FILESDIR`

#### Metadata extraction (PMS 7, 14)
- All 18 PMS metadata variables extracted after sourcing:
  `EAPI`, `DESCRIPTION`, `SLOT`, `HOMEPAGE`, `SRC_URI`, `LICENSE`, `KEYWORDS`,
  `IUSE`, `REQUIRED_USE`, `RESTRICT`, `PROPERTIES`, `DEPEND`, `RDEPEND`,
  `BDEPEND`, `PDEPEND`, `IDEPEND`, `INHERITED`, `DEFINED_PHASES`
- Comparison tooling: `examples/regen_cache.rs` sources every ebuild and diffs
  against the md5-cache

#### Portage-specific shell functions (PMS 12)
- `die`, `nonfatal`
- `has`, `hasv`, `hasq`
- `use`, `usev`, `usex`, `use_enable`, `use_with`, `in_iuse` (stubs — always return false)
- `ver_cut`, `ver_rs`, `ver_test` (buggy — see below)
- `has_version`, `best_version` (stubs)
- Debug/output no-ops: `einfo`, `ewarn`, `eerror`, `debug-print`, etc.
- Build/install stubs: `econf`, `emake`, `eapply`, `dobin`, `doins`, etc.

---

### Bugs

#### `__ver_split` treats letters as separators (PMS 12.3.14)
The version splitting helper only recognises `[0-9]+` as components and treats
everything else as separators. PMS says `[A-Za-z]+` sequences are version
*components*, not separators, and empty-string separators occur at digit↔letter
transitions. For example `1.2a3` should produce components `[1, 2, a, 3]` with
separators `[., "", ""]` but the current code treats `a` as part of a separator.

#### `ver_test` comparison is numeric-only (PMS 3.3, 12.3.14)
`ver_test` compares only numeric components. It ignores:
- Letter components (e.g. `1.0a` vs `1.0b`)
- Suffixes (`_alpha`, `_beta`, `_pre`, `_rc`, `_p`)
- Revision comparison (`-r1` vs `-r2`)

PMS requires the full algorithm 3.1 (version comparison).

#### `ver_test` 2-arg form uses `${PV}` instead of `${PVR}` (PMS 12.3.14)
When called with two arguments (`ver_test <op> <v2>`), the LHS should default
to `${PVR}`, not `${PV}`.

#### `ver_rs` only handles one range/replacement pair (PMS 12.3.14)
PMS says `ver_rs` takes "one or more pairs of arguments, optionally followed by
a version string." The implementation only handles a single pair.

#### `ver_cut` zero-index is broken (PMS 12.3.14)
Range index 0 should refer to the separator before the first component. The
arithmetic `(0 - 1) * 2 = -2` produces an invalid array index.

#### `profiles.desc` rejects unknown stability values (PMS 4.4.1)
`ProfileStatus::parse()` only accepts `stable`, `dev`, `exp`. PMS allows
repositories to define additional values.

#### `PR` variable not set (PMS 11.1)
Ebuilds referencing `${PR}` at global scope get an empty string. PMS requires
`PR` to be `r0` when no revision exists, or `rN` otherwise.

---

### Missing features

#### PM-provided variables (PMS 11.1)
Only `CATEGORY`, `PN`, `PV`, `PVR`, `P`, `PF`, `FILESDIR` are set. Missing:
- `PR` (bug, see above)
- `WORKDIR`, `S`, `T`, `TMPDIR`, `HOME` — needed for ebuilds that reference
  these at global scope
- `D`, `ED`, `ROOT`, `EROOT`, `EPREFIX`, `DISTDIR` — phase-execution only
- `SYSROOT`, `ESYSROOT`, `BROOT` — EAPI 7+
- `EBUILD_PHASE`, `EBUILD_PHASE_FUNC`, `MERGE_TYPE`

#### DEFINED_PHASES not computed (PMS 7.4)
The shell does not scan for defined phase functions after sourcing.
`DEFINED_PHASES` is whatever the ebuild/eclasses set explicitly (typically
nothing, showing as `-`). Portage computes this by inspecting which functions
exist in the shell after sourcing.

#### EAPI pre-source detection (PMS 7.3.1)
PMS requires detecting EAPI by regex-matching the first assignment line
*before* sourcing. The code sources the ebuild and reads EAPI from the shell
environment afterward.

#### Profile inheritance / stacking (PMS 5.1, 5.2.5)
`Profile` reads files in isolation — no parent merging, no `-` prefix removal
for incremental files.

#### Directory-as-file profile support (PMS 5.2.5)
For EAPI 7+ with `profile-file-dirs`, `package.mask`, `package.use`, `use.*`,
and `package.use.*` can be directories containing multiple files. Not handled.

#### `deprecated` profile file (PMS 5.2.3)
No method to check or read the `deprecated` file.

#### `use.stable` / `package.use.stable` (PMS 5.2.11)
These EAPI 9 profile files are not implemented.

#### Top-level `profiles/eapi` (PMS 4.4)
EAPI 9 allows a `profiles/eapi` file that sets the default EAPI for profiles.
Not read.

#### Repository-level `profiles/package.mask` (PMS 4.4)
Not read (only profile-level `package.mask` is handled).

#### `profiles/desc/` directory (PMS 4.4)
USE_EXPAND variable descriptions not implemented.

#### `profiles/updates/` directory (PMS 4.4.4)
Package move/slotmove updates not implemented.

#### Master repository eclass resolution (PMS 4.7, 10.1)
`layout.conf` `masters` is parsed but not used to add master repos' eclass
directories. Ebuilds in overlay repos that inherit from `::gentoo` eclasses
will fail to find them.

#### Eclass metadata key accumulation (PMS 10.2)
Eclasses that overwrite (rather than append to) `DEPEND`, `RDEPEND`, etc. are
not corrected. PMS requires the PM to save/restore/append these keys across
`inherit` calls.

#### Legacy metadata cache format (PMS 14.2)
Only md5-dict (`metadata/md5-cache/`) is supported. The positional line-based
`metadata/cache/` format is not implemented.

#### CVS directory exclusion (PMS 4.2)
Dotfiles are skipped but `CVS` directories are not explicitly excluded from
category/package enumeration.

#### `ver_replacing` command (PMS 12.3.14, EAPI 9)
Not implemented.

#### Bash compatibility per EAPI (PMS 6, Table 6.1)
`BASH_COMPAT` is not set per EAPI. PMS requires bash 3.2 for EAPIs 0–5,
4.2 for EAPIs 6–7, 5.0 for EAPI 8, 5.3 for EAPI 9.

#### `failglob` in global scope (PMS 6)
For EAPIs 6+, the `failglob` option should be set in global scope. Not done.

---

### Upstream dependencies

#### portage-metadata / portage-atom parsing gaps
The metadata and dependency parsers reject several valid PMS constructs,
causing ~2.5% of ebuilds to fail during `CacheEntry::parse()`. See
`../portage-metadata/ISSUES.md` for details.

#### brush-core parser bugs
Some ebuilds fail to parse due to remaining brush-core/winnow bugs.
See `../brush/ISSUES.md` for details. Remaining open issues:
- `<<-` tab stripping inside command substitutions
- Complex parameter expansion edge cases
- Arithmetic expansion edge cases

---

### Serialization ordering differences
`CacheEntry::serialize()` in portage-metadata may produce fields in a different
order or with different whitespace than the reference cache from `pmaint regen`.
The regen_cache comparison uses key-by-key diffing, but within-value ordering
(e.g. USE flags, keywords) may still cause false-positive diffs.

### USE flag stubs always return false
`use()`, `usev()`, `usex()` always return 1 (false). Correct for metadata
extraction (no profile active), but ebuilds that conditionally set metadata
variables based on USE flags at source time will produce different values.

### Missing `tc-*` and other toolchain-funcs
Eclasses like `toolchain-funcs.eclass` define functions (`tc-getCC`,
`tc-is-gcc`, etc.) that some ebuilds call at source time. These are handled by
sourcing the eclass, but any that shell out to real compilers will fail.

## Running the full comparison

```bash
# Single ebuild
cargo run --release --example regen_cache -- gentoo 'dev-lang/rust-1.88.0'

# Whole category
cargo run --release --example regen_cache -- gentoo 'dev-lang/*'

# Full tree (~32K ebuilds, slow)
cargo run --release --example regen_cache -- gentoo
```

Output goes to stderr (progress + errors + diffs) and stdout (final stats).
Exit code is 1 if there are any errors or mismatches.
