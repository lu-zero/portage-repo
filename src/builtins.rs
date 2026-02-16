//! Portage-specific bash function definitions for the embedded shell.
//!
//! Real ebuilds and eclasses expect a set of Portage-provided functions
//! (`inherit`, `die`, `EXPORT_FUNCTIONS`, etc.) to exist at source time.
//! Rather than implementing each as a Rust builtin, we define them as
//! bash shell functions via [`brush_core::Shell::run_string`].
//!
//! See [PMS 10](https://projects.gentoo.org/pms/9/pms.html#eclasses)
//! and [PMS 12](https://projects.gentoo.org/pms/9/pms.html#available-commands) for the
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
# See PMS 10 — eclasses.
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
# See PMS 10 — EXPORT_FUNCTIONS.
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
# Implements PMS 12.3.14 version manipulation commands and
# PMS 3.3 algorithm 3.1 for version comparison.

# __ver_split: split version string into components and separators
# per PMS 12.3.14.
#
# Sets __ver[] as [sep0, comp1, sep1, comp2, sep2, ..., compN, sepN]
# and __ver_ncomp with the number of components.
#
# Components: [0-9]+ or [A-Za-z]+
# Separators: [^A-Za-z0-9]+, or empty at digit↔letter transitions
__ver_split() {
    local v="$1" LC_ALL=C
    __ver=()
    __ver_ncomp=0

    local s c
    while [[ -n "${v}" ]]; do
        # Cut the separator
        if [[ "${v}" =~ ^([^A-Za-z0-9]+) ]]; then
            s="${BASH_REMATCH[1]}"
        else
            s=""
        fi
        v="${v:${#s}}"
        # Cut the next component: digits or letters
        if [[ "${v}" == [0-9]* ]]; then
            if [[ "${v}" =~ ^([0-9]+) ]]; then
                c="${BASH_REMATCH[1]}"
            else
                c=""
            fi
        else
            if [[ "${v}" =~ ^([A-Za-z]+) ]]; then
                c="${BASH_REMATCH[1]}"
            else
                c=""
            fi
        fi
        v="${v:${#c}}"
        __ver+=("${s}" "${c}")
        [[ -n "${c}" ]] && __ver_ncomp=$(( __ver_ncomp + 1 ))
    done
}

# __ver_parse_range: parse "M", "M-N", "M-" into start/end
# Usage: __ver_parse_range <range> <max>
__ver_parse_range() {
    local range="$1" max="$2"
    [[ "${range}" == [0-9]* ]] \
        || { die "${FUNCNAME}: range must start with a number"; return 1; }
    __range_start="${range%-*}"
    if [[ "${range}" == *-* ]]; then
        __range_end="${range#*-}"
    else
        __range_end="${__range_start}"
    fi
    if [[ -n "${__range_end}" ]]; then
        (( __range_start <= __range_end )) \
            || { die "${FUNCNAME}: end of range must be >= start"; return 1; }
        (( __range_end <= max )) || __range_end=${max}
    else
        __range_end=${max}
    fi
}

# ver_cut <range> [<version>]
# PMS 12.3.14: extract version components by range
ver_cut() {
    local range="$1"
    local v="${2:-${PV}}"
    local __range_start __range_end
    local -a __ver
    local __ver_ncomp

    __ver_split "${v}"
    local max=$(( ${#__ver[@]} / 2 ))
    __ver_parse_range "${range}" "${max}" || return
    local start=${__range_start} end=${__range_end}

    if (( start > 0 )); then
        start=$(( start * 2 - 1 ))
    fi
    # Work around a bug in bash-3.2, where "${__ver[*]:start:end*2-start}"
    # inserts stray 0x7f characters for empty array elements
    printf "%s" "${__ver[@]:start:end*2-start}" $'\n'
}

# ver_rs <range> <repl> [<range> <repl>]... [<version>]
# PMS 12.3.14: replace version separators (one or more pairs)
ver_rs() {
    local v
    (( $# & 1 )) && v="${@: -1}" || v="${PV}"
    local __range_start __range_end i
    local -a __ver
    local __ver_ncomp

    __ver_split "${v}"
    local max=$(( ${#__ver[@]} / 2 - 1 ))

    while [[ $# -ge 2 ]]; do
        __ver_parse_range "$1" "${max}" || return
        for (( i = __range_start * 2; i <= __range_end * 2; i += 2 )); do
            [[ ${i} -eq 0 && -z "${__ver[i]}" ]] && continue
            __ver[i]="$2"
        done
        shift 2
    done

    local result=""
    for (( i = 0; i < ${#__ver[@]}; i++ )); do
        result+="${__ver[i]}"
    done
    echo "${result}"
}

# __ver_compare_int: compare two non-negative integers of arbitrary length
# Returns: 0 if equal, 1 if a < b, 3 if a > b
__ver_compare_int() {
    local a="$1" b="$2" d=$(( ${#1} - ${#2} ))

    # Zero-pad to equal length if necessary
    if [[ ${d} -gt 0 ]]; then
        printf -v b "%0${d}d%s" 0 "${b}"
    elif [[ ${d} -lt 0 ]]; then
        printf -v a "%0$(( -d ))d%s" 0 "${a}"
    fi

    [[ "${a}" > "${b}" ]] && return 3
    [[ "${a}" == "${b}" ]]
}

# __ver_compare: PMS algorithm 3.1 full version comparison
# Returns: 1 if va < vb, 2 if va == vb, 3 if va > vb
__ver_compare() {
    local va="$1" vb="$2" a an al as ar b bn bl bs br re LC_ALL=C

    re="^([0-9]+(\.[0-9]+)*)([a-z]?)((_(alpha|beta|pre|rc|p)[0-9]*)*)(-r[0-9]+)?$"

    [[ "${va}" =~ ${re} ]] || { die "${FUNCNAME}: invalid version: ${va}"; return 0; }
    an="${BASH_REMATCH[1]}"
    al="${BASH_REMATCH[3]}"
    as="${BASH_REMATCH[4]}"
    ar="${BASH_REMATCH[7]}"

    [[ "${vb}" =~ ${re} ]] || { die "${FUNCNAME}: invalid version: ${vb}"; return 0; }
    bn="${BASH_REMATCH[1]}"
    bl="${BASH_REMATCH[3]}"
    bs="${BASH_REMATCH[4]}"
    br="${BASH_REMATCH[7]}"

    # Compare numeric components (PMS algorithm 3.2)
    # First component
    __ver_compare_int "${an%%.*}" "${bn%%.*}" || return

    while [[ "${an}" == *.* && "${bn}" == *.* ]]; do
        # Other components (PMS algorithm 3.3)
        an="${an#*.}"
        bn="${bn#*.}"
        a="${an%%.*}"
        b="${bn%%.*}"
        if [[ "${a}" == 0* || "${b}" == 0* ]]; then
            # Remove any trailing zeros
            [[ "${a}" =~ 0+$ ]] && a="${a%"${BASH_REMATCH[0]}"}"
            [[ "${b}" =~ 0+$ ]] && b="${b%"${BASH_REMATCH[0]}"}"
            [[ "${a}" > "${b}" ]] && return 3
            [[ "${a}" < "${b}" ]] && return 1
        else
            __ver_compare_int "${a}" "${b}" || return
        fi
    done
    [[ "${an}" == *.* ]] && return 3
    [[ "${bn}" == *.* ]] && return 1

    # Compare letter components (PMS algorithm 3.4)
    [[ "${al}" > "${bl}" ]] && return 3
    [[ "${al}" < "${bl}" ]] && return 1

    # Compare suffixes (PMS algorithm 3.5)
    as="${as#_}${as:+_}"
    bs="${bs#_}${bs:+_}"
    while [[ -n "${as}" && -n "${bs}" ]]; do
        # Compare each suffix (PMS algorithm 3.6)
        a="${as%%_*}"
        b="${bs%%_*}"
        if [[ "${a%%[0-9]*}" == "${b%%[0-9]*}" ]]; then
            __ver_compare_int "${a##*[a-z]}" "${b##*[a-z]}" || return
        else
            # Check for p first
            [[ "${a%%[0-9]*}" == "p" ]] && return 3
            [[ "${b%%[0-9]*}" == "p" ]] && return 1
            # Hack: Use that alpha < beta < pre < rc alphabetically
            [[ "${a}" > "${b}" ]] && return 3 || return 1
        fi
        as="${as#*_}"
        bs="${bs#*_}"
    done
    if [[ -n "${as}" ]]; then
        [[ "${as}" == p[_0-9]* ]] && return 3 || return 1
    elif [[ -n "${bs}" ]]; then
        [[ "${bs}" == p[_0-9]* ]] && return 1 || return 3
    fi

    # Compare revision components (PMS algorithm 3.7)
    __ver_compare_int "${ar#-r}" "${br#-r}" || return

    return 2
}

# ver_test [<v1>] <op> <v2>
# PMS 12.3.14: compare versions using PMS algorithm 3.1
ver_test() {
    local va op vb

    if [[ $# -eq 3 ]]; then
        va="$1"
        shift
    else
        va="${PVR}"
    fi

    [[ $# -eq 2 ]] || { die "${FUNCNAME}: bad number of arguments"; return 1; }

    op="$1"
    vb="$2"

    case "${op}" in
        -eq|-ne|-lt|-le|-gt|-ge) ;;
        *) die "${FUNCNAME}: invalid operator: ${op}"; return 1 ;;
    esac

    __ver_compare "${va}" "${vb}"
    test $? "${op}" 2
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
