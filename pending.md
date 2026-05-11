# Pending work: ebuild phase execution

Two-track plan: Rust builtins for real phase execution, plus brush compat
test cases that document current shell gaps.

---

## Track 1 — Brush compatibility test cases

Five YAML test files in `brush/brush-shell/tests/cases/compat/`,
committed on branch `for-portage-repo` in the brush repo.

- [x] `portage_gap1_bare_function_body.yaml` — bare `[[ ]]`/`(( ))` as
  function body; 2 of 3 cases fail (gap still open in brush parser)
- [x] `portage_gap2_extglob_in_brackets.yaml` — all pass (already fixed)
- [x] `portage_gap3_bash_rematch.yaml` — all pass (already fixed)
- [x] `portage_gap4_printf_v.yaml` — all pass (already fixed)
- [x] `portage_gap5_mapfile.yaml` — all pass (already fixed)

Gap 1 (bare compound-command function body) is the only remaining brush
parser limitation that affects portage code.  The 74 `___eapi_*` predicates
use that syntax; they are worked around by the `EapiPredicateCommand` Rust
builtin rather than being fixed in brush.

---

## Track 2 — Rust builtins for phase execution

`builtins.rs` registers bash no-op stubs so eclasses don't error during
metadata extraction (they call `econf`, `einfo`, etc. at source time).
`init_build_env()` unsets those stubs before any build phase so the Rust
builtins below take effect.

### P0 — Phase dispatch

- [x] `___eapi_*` — 74 EAPI predicate builtins, dispatched via
  `context.command_name` against a static `match`
- [x] `__ebuild_phase_funcs` — wires up `default()` / `default_<phase>()`
  and installs a fallback `<phase>()` when the ebuild didn't define it
- [x] `__eapi0_pkg_nofetch`, `__eapi0_src_unpack`, `__eapi0_src_compile`,
  `__eapi0_src_test`, `__eapi1_src_compile`, `__eapi2_src_prepare`,
  `__eapi2_src_configure`, `__eapi2_src_compile`, `__eapi4_src_install`,
  `__eapi6_src_prepare`, `__eapi6_src_install`, `__eapi8_src_prepare` —
  bash implementations in `PHASE_DEFAULT_FUNCTIONS`, called by
  `__ebuild_phase_funcs`

Known gaps in `__eapi0_src_test`:
- [ ] missing `-j1` for EAPI ≤ 4 (needs `___eapi_default_src_test_disables_parallel_jobs`)
- [ ] missing MAKEFLAGS jobserver guard (portage bug #692576)

Known gap in `EbuildPhaseFuncsCommand`:
- [ ] does not install `default_<other_phase>()` error stubs for phases
  other than the one currently executing (portage installs all of them)

### P1 — Output helpers

- [x] `einfo`, `elog`, `ewarn`, `eerror`, `eqawarn`, `einfon`
- [x] `ebegin`
- [x] `eend`

### P2 — Build helpers

- [x] `emake` — spawns `${MAKE:-make}` with `$MAKEOPTS $EXTRA_EMAKE`
- [x] `econf` — spawns `./configure` with EAPI-appropriate flags;
  probes `--help` for conditional flags with word-boundary guard
- [x] `assert` — bash function; captures `PIPESTATUS` before any other
  command to avoid clobbering
- [x] `nonfatal` — bash function `"$@"; return 0`
  - [ ] does not set `PORTAGE_NONFATAL=1` (harmless: our `die` builtin
    does not check it, but latent if die grows nonfatal support)
- [x] `eapply` — bash function; `patch -p1 < file` loop
- [x] `eapply_user` — stub (`:`)
- [x] `einstalldocs` — stub (`:`)
- [x] `get_libdir` — checks `LIBDIR_${ABI}`, defaults to `lib`
- [ ] `edo` — EAPI 9 only; not yet implemented

Known gaps shared by `emake` and `econf`:
- [ ] `MAKEOPTS` / `EXTRA_EMAKE` / `EXTRA_ECONF` are split on whitespace;
  quoted values with internal spaces (portage uses `eval` for `EXTRA_ECONF`,
  bug #457136) are not handled

### P3 — Install helpers

All currently bash no-op stubs in `builtins.rs`.  Each needs a real
implementation that installs into `${D}` with correct ownership/permissions.

- [ ] `into` / `insinto` / `exeinto` / `docinto`
- [ ] `insopts` / `exeopts`
- [ ] `dobin` / `newbin`
- [ ] `dosbin` / `newsbin`
- [ ] `doins` / `newins`
- [ ] `doexe` / `newexe`
- [ ] `dolib` / `dolib.a` / `dolib.so`
- [ ] `dodir` / `keepdir`
- [ ] `dodoc` / `newdoc`  (also needed by `__eapi4_src_install` DOCS handling)
- [ ] `doman` / `newman`
- [ ] `dosym` (EAPI 8 adds `-r` for relative symlinks)
- [ ] `doheader` / `newheader`
- [ ] `docompress`
- [ ] `dostrip`
- [ ] `__eapi4_src_install` DOCS: currently missing `dodoc` call after
  `make install`; any EAPI 4–5 package that sets `DOCS` will silently skip docs

### P4 — Unpack

- [ ] `unpack` — currently a bash stub that calls `die`; needs dispatch
  by extension: `.tar.*`, `.zip`, `.gz`, `.bz2`, `.xz`, `.zst`, `.7z`,
  `.rar`, `.lha` (EAPI ≤ 7 only for 7z/rar/lha per PMS)

### P5 — Package query stubs

Already wired as bash stubs; no Rust builtin needed until dep-solving is in scope.

- [x] `has_version` — returns 1
- [x] `best_version` — returns empty string + 1

---

## End-to-end test

To exercise the full configure → compile → install flow:

```bash
# Download and unpack source manually (unpack not yet implemented)
mkdir -p /tmp/hello-build/work && cd /tmp/hello-build/work
wget https://ftp.gnu.org/gnu/hello/hello-2.12.2.tar.gz
tar xf hello-2.12.2.tar.gz

# Run phases
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 configure --work-dir /tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 compile    --work-dir /tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 install    --work-dir /tmp/hello-build
```

Once P4 (`unpack`) is done, the sequence becomes:

```bash
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 unpack    --work-dir /tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 configure --work-dir /tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 compile   --work-dir /tmp/hello-build
cargo run --example ebuild -- /var/db/repos/gentoo app-misc/hello-2.12.2 install   --work-dir /tmp/hello-build
```
