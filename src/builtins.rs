//! Portage-specific bash function definitions for the embedded shell.
//!
//! Real ebuilds and eclasses expect a set of Portage-provided functions
//! (`inherit`, `die`, `EXPORT_FUNCTIONS`, etc.) to exist at source time.
//! Rather than implementing each as a Rust builtin, we define them as
//! bash shell functions via [`brush_core::Shell::run_string`].
//!
//! See [PMS 11](https://projects.gentoo.org/pms/latest/pms.html#ebuild-phase-functions)
//! and [PMS 12](https://projects.gentoo.org/pms/latest/pms.html#eclasses) for the
//! functions an ebuild/eclass may call.

use brush_core::{Shell, SourceInfo};

use crate::error::{Error, Result};

/// Register all Portage-specific shell functions in the given shell.
///
/// This must be called once during [`crate::EbuildShell::new`] before
/// any ebuild or eclass is sourced.
pub(crate) async fn register(shell: &mut Shell) -> Result<()> {
    let params = shell.default_exec_params();
    let source_info = SourceInfo::from("portage-builtins");
    shell
        .run_string(PORTAGE_FUNCTIONS, &source_info, &params)
        .await
        .map_err(|e| Error::Shell(format!("registering portage builtins: {e}")))?;
    Ok(())
}

/// All Portage-specific bash function definitions, concatenated into a
/// single script that is evaluated once at shell init time.
const PORTAGE_FUNCTIONS: &str = r#"
# ── Tier 1: critical for eclass/ebuild sourcing ──────────────────────

# die: abort with error message
die() {
    echo "die: $*" >&2
    return 1
}

# nonfatal: run command, ignore failure
nonfatal() { "$@"; return 0; }

# inherit: source eclasses, track INHERITED, manage ECLASS
#
# See PMS 12 — eclasses.
# Eclass directories are communicated via the colon-separated
# __PORTAGE_ECLASS_DIRS variable set by the Rust host.
inherit() {
    local __eclass
    for __eclass in "$@"; do
        # Skip if already inherited
        case " ${INHERITED} " in
            *" ${__eclass} "*) continue ;;
        esac
        local __eclass_file=""
        local __dir
        local IFS=:
        for __dir in ${__PORTAGE_ECLASS_DIRS}; do
            if [[ -f "${__dir}/${__eclass}.eclass" ]]; then
                __eclass_file="${__dir}/${__eclass}.eclass"
                break
            fi
        done
        unset IFS
        if [[ -z "${__eclass_file}" ]]; then
            die "inherit: eclass not found: ${__eclass}"
            return 1
        fi
        local __prev_eclass="${ECLASS}"
        ECLASS="${__eclass}"
        source "${__eclass_file}" || die "inherit: failed to source ${__eclass}"
        ECLASS="${__prev_eclass}"
        INHERITED="${INHERITED:+${INHERITED} }${__eclass}"
    done
}

# EXPORT_FUNCTIONS: create phase aliases for the current eclass
#
# See PMS 12 — EXPORT_FUNCTIONS.
EXPORT_FUNCTIONS() {
    if [[ -z "${ECLASS}" ]]; then
        die "EXPORT_FUNCTIONS called outside eclass scope"
        return 1
    fi
    local __phase
    for __phase in "$@"; do
        eval "${__phase}() { ${ECLASS}_${__phase} \"\$@\"; }"
    done
}

# ── Tier 2: called at eclass source time ─────────────────────────────

# Debug output (no-ops for metadata extraction)
debug-print()          { :; }
debug-print-function() { :; }
debug-print-section()  { :; }

# User output (no-ops for metadata extraction)
einfo()   { :; }
einfon()  { :; }
ewarn()   { :; }
eerror()  { :; }
elog()    { :; }
eqawarn() { :; }
ebegin()  { :; }
eend()    { return "${1:-0}"; }

# has: check if needle is in haystack
has() {
    local __needle="$1"; shift
    local __x
    for __x in "$@"; do
        [[ "${__x}" == "${__needle}" ]] && return 0
    done
    return 1
}

hasv() {
    local __needle="$1"; shift
    local __x
    for __x in "$@"; do
        if [[ "${__x}" == "${__needle}" ]]; then
            echo "${__needle}"
            return 0
        fi
    done
    return 1
}

hasq() { has "$@"; }

# ── Tier 3: USE flag queries (stubs for metadata extraction) ─────────

use()        { return 1; }
usev()       { return 1; }
usex()       { [[ $# -ge 3 ]] && echo "$3" || echo "no"; return 1; }
use_enable() { echo "--disable-${2:-$1}"; }
use_with()   { echo "--without-${2:-$1}"; }
in_iuse()    { has "$1" ${IUSE}; }

# ── Tier 4: version manipulation (EAPI 7+ PM-provided) ──────────────
#
# Ported from the PMS eapi7-ver reference implementation.

# __ver_split: split version string into alternating components
# e.g. "1.2.3_alpha4-r1" → array of (number sep number sep ...)
__ver_split() {
    local ver="$1"
    __ver_components=()
    while [[ -n "${ver}" ]]; do
        # grab leading digits
        if [[ "${ver}" =~ ^([0-9]+)(.*) ]]; then
            __ver_components+=("${BASH_REMATCH[1]}")
            ver="${BASH_REMATCH[2]}"
        fi
        # grab leading non-digits (separator)
        if [[ "${ver}" =~ ^([^0-9]+)(.*) ]]; then
            __ver_components+=("${BASH_REMATCH[1]}")
            ver="${BASH_REMATCH[2]}"
        fi
    done
}

# ver_cut: extract version components
#   ver_cut <range> [<version>]
# range is M or M-N (1-based, components only, not separators)
ver_cut() {
    local range="$1"
    local ver="${2:-${PV}}"
    local __ver_components
    __ver_split "${ver}"

    local start end
    if [[ "${range}" == *-* ]]; then
        start="${range%%-*}"
        end="${range##*-}"
    else
        start="${range}"
        end="${range}"
    fi
    [[ -z "${start}" ]] && start=1
    [[ -z "${end}" ]] && end=${#__ver_components[@]}

    # Convert component indices to array indices
    # Component 1 = array[0], separator after 1 = array[1], component 2 = array[2], etc.
    local s_idx=$(( (start - 1) * 2 ))
    local e_idx=$(( (end - 1) * 2 ))

    local result=""
    local i
    for (( i = s_idx; i <= e_idx && i < ${#__ver_components[@]}; i++ )); do
        result+="${__ver_components[i]}"
    done
    echo "${result}"
}

# ver_rs: replace version separators
#   ver_rs <range> <replacement> [<version>]
ver_rs() {
    local range="$1"
    local repl="$2"
    local ver="${3:-${PV}}"
    local __ver_components
    __ver_split "${ver}"

    local start end
    if [[ "${range}" == *-* ]]; then
        start="${range%%-*}"
        end="${range##*-}"
    else
        start="${range}"
        end="${range}"
    fi
    [[ -z "${start}" ]] && start=1
    [[ -z "${end}" ]] && end=$(( ${#__ver_components[@]} / 2 ))

    # Separators are at odd indices: sep after component N is at array index (N*2 - 1)
    local i
    for (( i = start; i <= end; i++ )); do
        local idx=$(( i * 2 - 1 ))
        if (( idx < ${#__ver_components[@]} )); then
            __ver_components[idx]="${repl}"
        fi
    done

    local result=""
    for (( i = 0; i < ${#__ver_components[@]}; i++ )); do
        result+="${__ver_components[i]}"
    done
    echo "${result}"
}

# ver_test: compare two versions
#   ver_test [<v1>] <op> <v2>
ver_test() {
    local va op vb
    if [[ $# -eq 3 ]]; then
        va="$1"; op="$2"; vb="$3"
    elif [[ $# -eq 2 ]]; then
        va="${PV}"; op="$1"; vb="$2"
    else
        die "ver_test: invalid arguments: $*"
        return 1
    fi

    local __ver_components
    __ver_split "${va}"
    local a_comps=("${__ver_components[@]}")
    __ver_split "${vb}"
    local b_comps=("${__ver_components[@]}")

    # Compare component by component (numeric components only, at even indices)
    local max=${#a_comps[@]}
    (( ${#b_comps[@]} > max )) && max=${#b_comps[@]}

    local i result=0
    for (( i = 0; i < max; i += 2 )); do
        local ac="${a_comps[i]:-0}"
        local bc="${b_comps[i]:-0}"
        if (( ac < bc )); then
            result=-1; break
        elif (( ac > bc )); then
            result=1; break
        fi
    done

    case "${op}" in
        -eq) (( result == 0 )) ;;
        -ne) (( result != 0 )) ;;
        -lt) (( result < 0 )) ;;
        -le) (( result <= 0 )) ;;
        -gt) (( result > 0 )) ;;
        -ge) (( result >= 0 )) ;;
        *)
            die "ver_test: unknown operator: ${op}"
            return 1
            ;;
    esac
}

# ── Tier 5: package query stubs ──────────────────────────────────────

has_version()  { return 1; }
best_version() { echo ""; return 1; }

# ── Tier 6: build/install command stubs ──────────────────────────────

econf()   { :; }
emake()   { :; }
einstall() { :; }
unpack()  { :; }
eapply()  { :; }
eapply_user() { :; }
default() { :; }
default_src_unpack()    { :; }
default_src_prepare()   { :; }
default_src_configure() { :; }
default_src_compile()   { :; }
default_src_install()   { :; }
default_src_test()      { :; }

# Directory commands
into()    { :; }
insinto() { :; }
exeinto() { :; }

# Install commands
dobin()    { :; }
dosbin()   { :; }
doins()    { :; }
doman()    { :; }
dodoc()    { :; }
doheader() { :; }
dolib.a()  { :; }
dolib.so() { :; }
newbin()   { :; }
newins()   { :; }
dosym()    { :; }
dodir()    { :; }
keepdir()  { :; }
doexe()    { :; }
doinitd()  { :; }
doconfd()  { :; }
fperms()   { :; }
fowners()  { :; }
"#;
