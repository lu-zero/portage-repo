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

#### Master repository eclass resolution (PMS 4.7, 10.1)
- `Repository::open_with_masters()` — opens a repo and recursively resolves
  master repositories from a base directory (depth-first, with cycle detection)
- `Repository::shell_with_masters()` — creates an `EbuildShell` with master
  eclass directories prepended, so `inherit` finds eclasses from masters

#### Embedded bash shell (PMS 10, 12)
- Full embedded shell via brush-core with the winnow parser
- Eclass sourcing via `inherit()` with `INHERITED` tracking and `ECLASS` scoping
- PMS 10.2 eclass metadata key accumulation: `IUSE`, `REQUIRED_USE`, `DEPEND`,
  `BDEPEND`, `RDEPEND`, `PDEPEND`, `IDEPEND` (all EAPIs), plus `PROPERTIES` and
  `RESTRICT` (EAPI 8+) are saved/cleared/restored around each eclass `source`
- `EXPORT_FUNCTIONS` phase alias creation
- PM-provided variables: `CATEGORY`, `PN`, `PV`, `PVR`, `P`, `PF`, `PR`, `FILESDIR`,
  `EBUILD`, `WORKDIR`, `S`, `T`, `TMPDIR`, `HOME`, `D`, `DISTDIR`,
  `EBUILD_PHASE`, `EBUILD_PHASE_FUNC`, `ROOT`, `MERGE_TYPE`,
  `EPREFIX`/`ED`/`EROOT` (EAPI 3+), `SYSROOT`/`ESYSROOT`/`BROOT` (EAPI 7+)

#### Metadata extraction (PMS 7, 14)
- All 18 PMS metadata variables extracted after sourcing:
  `EAPI`, `DESCRIPTION`, `SLOT`, `HOMEPAGE`, `SRC_URI`, `LICENSE`, `KEYWORDS`,
  `IUSE`, `REQUIRED_USE`, `RESTRICT`, `PROPERTIES`, `DEPEND`, `RDEPEND`,
  `BDEPEND`, `PDEPEND`, `IDEPEND`, `INHERITED`, `DEFINED_PHASES`
- `EAPI` detected by regex before sourcing per PMS 7.3.1 and set in the shell
  environment so it is available during sourcing
- `DEFINED_PHASES` computed from shell function table after sourcing (PMS 7.4)
- Comparison tooling: `examples/regen_cache.rs` sources every ebuild and diffs
  against the md5-cache

#### Portage-specific shell functions (PMS 12)
- `die`, `nonfatal`
- `has`, `hasv`, `hasq`
- `use`, `usev`, `usex`, `use_enable`, `use_with`, `in_iuse` (stubs — always return false)
- `ver_cut`, `ver_rs`, `ver_test` (match Gentoo reference implementation)
- `has_version`, `best_version` (stubs)
- Debug/output no-ops: `einfo`, `ewarn`, `eerror`, `debug-print`, etc.
- Build/install stubs: `econf`, `emake`, `eapply`, `dobin`, `doins`, etc.

---

### Missing features

#### PM-provided variables (PMS 11.1)
All global-scope PM-provided variables are now set.  Phase-specific accuracy
is still approximate (e.g. `EBUILD_PHASE` is always `depend`, `MERGE_TYPE` is
always `source`) since this codebase only does metadata extraction.

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

#### Legacy metadata cache format (PMS 14.2)
Only md5-dict (`metadata/md5-cache/`) is supported. The positional line-based
`metadata/cache/` format is not implemented.

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
