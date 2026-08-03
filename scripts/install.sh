#!/usr/bin/env bash
#
# Tuni installer and updater.
#
#     bash <(curl -fsSL https://raw.githubusercontent.com/unisic/tuni/main/scripts/install.sh)
#
# or, from a checkout:
#
#     scripts/install.sh
#
# Tuni is not in COPR, the AUR, or any other package repository yet, so a
# release does not arrive through the system's own updates. This script is what
# stands in for that: it asks the GitHub release page what the newest version
# is, and installs the package built for this distribution with the machine's
# own package manager, the RPM on Fedora, the deb on Debian and Ubuntu, the
# pkg.tar.zst on Arch. Installing over an older copy is what an update is, so
# there is one code path and not two.
#
# Run with no arguments it opens a menu, because the person running it is at a
# terminal by definition and a menu can say what is installed, offer an older
# version and remove the thing again. The flags are for everything that is not
# a person:
#
#     install.sh -y           install or update, no questions (what Tuni's own
#                             "Update" button runs, in a tab of its own)
#     install.sh --check      say what is installed and what is newest, then
#                             exit 10 if an update is available
#
# There is deliberately no background timer. Every route here writes to /usr
# and therefore wants a password, and a timer has nowhere to ask for one; the
# check belongs in Tuni, which has a window to ask in and a terminal to run
# this in. Turn it off under Preferences, Terminal, Updates.
#
# A Flatpak install is left alone: the sandbox cannot install packages on the
# host, and the manifest in packaging/ is a local build rather than a Flathub
# app, so its update is whatever built it.

set -euo pipefail

REPO="unisic/tuni"
API_BASE="https://api.github.com/repos/${REPO}"
# Overridable so a release page can be faked (a file:// URL works) while
# testing the dialog Tuni shows. Tuni reads the same variable.
API="${TUNI_UPDATE_API:-${API_BASE}/releases/latest}"
FLATPAK_ID="dev.unisic.Tuni"

ASSUME_YES=0
CHECK_ONLY=0
PURGE=0
ACTION=install
REQ_TAG=""            # a version picked from the menu, empty for "the newest"
MENU_CHOICE=""
IN_ALT=0              # 1 while the alternate screen owns the terminal

case "${1:-}" in
    -y|--yes)      ASSUME_YES=1 ;;
    --check)       CHECK_ONLY=1 ;;
    # Not `sed "$0"`: piped from curl there is no file to read the comment out
    # of, and $0 is the shell itself.
    -h|--help)     printf 'Tuni installer and updater.\n\n  install.sh          open the menu\n  install.sh -y       install or update, no questions\n  install.sh --check  report the versions; exit 10 if an update is available\n'; exit 0 ;;
    "")            : ;;
    *)             printf 'unknown option: %s (try --help)\n' "$1" >&2; exit 2 ;;
esac

say()  { printf '\033[1;35m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
have() { command -v "$1" >/dev/null 2>&1; }

# The whole interactive run, menu and install output alike, happens on the
# terminal's alternate screen the way top and less do. Leaving it puts back the
# command the user typed, so only the closing line survives.
enter_alt() {
    [ "$IN_ALT" -eq 1 ] && return 0
    printf '\033[?1049h\033[?25h\033[2J\033[H' >/dev/tty 2>/dev/null || true
    IN_ALT=1
}
leave_alt() {
    [ "$IN_ALT" -eq 0 ] && return 0
    printf '\033[?25h\033[?1049l' >/dev/tty 2>/dev/null || true
    IN_ALT=0
}

# The normal screen has to come back before the message, or the error would be
# wiped along with the alternate buffer.
die() { leave_alt; printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

tmpdir=""
_cleanup() {
    leave_alt
    [ -n "$tmpdir" ] && rm -rf "$tmpdir" 2>/dev/null || true
}
trap _cleanup EXIT

fetch() {
    if have curl; then curl -fsSL "$1"
    elif have wget; then wget -qO- "$1"
    else die "This needs curl or wget, and neither is installed."; fi
}

download() {
    say "Downloading $(basename "$2")"
    if have curl; then curl -fL --progress-bar -o "$2" "$1"
    elif have wget; then wget -q --show-progress -O "$2" "$1"
    else die "This needs curl or wget, and neither is installed."; fi
}

# Installing a package needs root. sudo asks for the login password, and it has
# a terminal to ask in because this only ever runs in one.
priv() {
    if [ "$(id -u)" -eq 0 ]; then "$@"
    elif have sudo; then sudo "$@"
    else die "Installing a package needs root, and sudo is not installed here."; fi
}

# Which package manager owns this machine, and therefore which asset fits.
# Only the three the release workflow builds: anything else has to build from
# source, and saying so is better than installing a package that will not run.
manager=""
ID=""; ID_LIKE=""
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
fi
if   have dnf    && { [ "$ID" = fedora ] || case " $ID_LIKE " in *" fedora "*) true ;; *) false ;; esac; }; then
    manager=dnf
elif have apt-get && { [ "$ID" = debian ] || [ "$ID" = ubuntu ] || case " $ID_LIKE " in *" debian "*) true ;; *) false ;; esac; }; then
    manager=apt
elif have pacman && { [ "$ID" = arch ] || case " $ID_LIKE " in *" arch "*) true ;; *) false ;; esac; }; then
    manager=pacman
fi

# What is on this machine now, or nothing. The package manager is asked rather
# than the binary, because a version is what a package records and `tuni` has
# no flag that prints one.
installed_version() {
    if have rpm && rpm -q tuni >/dev/null 2>&1; then
        rpm -q --qf '%{VERSION}' tuni
    elif have dpkg-query && dpkg-query -W -f='${Version}' tuni >/dev/null 2>&1; then
        dpkg-query -W -f='${Version}' tuni 2>/dev/null | cut -d- -f1
    elif have pacman && pacman -Qq tuni >/dev/null 2>&1; then
        pacman -Q tuni 2>/dev/null | awk '{print $2}' | cut -d- -f1
    fi
}

flatpak_installed() {
    have flatpak && flatpak info "$FLATPAK_ID" >/dev/null 2>&1
}

# One line for the menu's status bar. Local queries only, no network, so the
# menu opens instantly.
installed_status() {
    local v
    v="$(installed_version || true)"
    if [ -n "$v" ]; then printf 'Tuni %s installed (system package)' "$v"
    elif flatpak_installed; then printf 'Tuni installed (Flatpak)'
    else printf 'not-installed'; fi
}

# 0 when $1 is older than $2. `sort -V` is what puts 1.9 before 1.10; a string
# compare gets that pair backwards.
older_than() {
    [ -n "$1" ] && [ -n "$2" ] && [ "$1" != "$2" ] \
        && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1)" = "$1" ]
}

# The first browser_download_url whose filename matches the ERE in $1. Avoids a
# jq dependency, which a fresh machine has no reason to have.
asset_url() {
    grep -oE '"browser_download_url": *"[^"]+"' \
        | sed -E 's/.*"(https[^"]+)"/\1/' \
        | grep -E "$1" | head -n1 || true
}

tag_of() { printf '%s' "$1" | grep -oE '"tag_name": *"[^"]+"' | head -n1 | sed -E 's/.*"v?([^"]+)"$/\1/'; }

# ======================================================================
# btop-style bordered menu. ONE window (the alternate screen the caller
# entered) that morphs between views. Full redraw from the top on every key,
# no in-place cursor math, so a list that changes length cannot smear. All
# output goes to /dev/tty; the content is ASCII so the borders stay aligned.
# Sets MENU_CHOICE, and for a picked version REQ_TAG as well.
# ======================================================================
tui_run() {
    local tty=/dev/tty saved key rest act id n cols BOX_W INSTALLED
    exec 3<"$tty" || { warn "no terminal for the menu"; MENU_CHOICE=quit; return; }
    saved="$(stty -g <&3 2>/dev/null || true)"
    stty -echo -icanon min 1 time 0 <&3 2>/dev/null || true
    printf '\033[?25l' >"$tty"          # hide the cursor; the alt screen is already up
    trap 'stty "$saved" <&3 2>/dev/null || true; leave_alt; exit 130' INT

    cols="$(stty size <&3 2>/dev/null | awk '{print $2}')"
    [ -n "${cols:-}" ] || cols=80
    BOX_W=$(( cols - 4 ))
    [ "$BOX_W" -gt 74 ] && BOX_W=74
    [ "$BOX_W" -lt 48 ] && BOX_W=48

    local view=main sel=0 status_line status_style=dim qhint=quit
    local -a ids=() labels=() helps=() hdr=() VER_TAGS=()

    # The icon's own palette, so the installer looks like the thing it installs.
    local CB='\033[38;2;99;91;150m'                        # border
    local CT='\033[1;38;2;200;172;214m'                    # title
    local CS='\033[48;2;200;172;214m\033[38;2;23;21;59m'   # selected row
    local CD='\033[2m'                                     # dim
    local CO='\033[38;2;120;200;140m'                      # installed
    local R='\033[0m'

    _hr() { local nn="$1" i ss=''; for ((i=0; i<nn; i++)); do ss+='─'; done; printf '%s' "$ss"; }

    # One padded content line inside the box, in the given style.
    _boxline() {
        local txt="$1" style="$2" inner=$(( BOX_W - 2 )) pad c=''
        txt="${txt:0:$inner}"
        printf -v pad '%-*s' "$inner" "$txt"
        case "$style" in
            title) c="$CT" ;;
            sel)   c="$CS" ;;
            dim)   c="$CD" ;;
            ok)    c="$CO" ;;
        esac
        printf "${CB}│${R}%b%s${R}${CB}│${R}\r\n" "$c" "$pad" >"$tty"
    }
    _rule() { printf "${CB}%s%s%s${R}\r\n" "$1" "$(_hr $(( BOX_W - 2 )))" "$2" >"$tty"; }

    _draw() {
        local m=${#ids[@]} i
        printf '\033[H' >"$tty"
        _rule '╭' '╮'
        _boxline ' Tuni  installer' title
        _boxline ' a terminal workspace for Linux' dim
        _rule '├' '┤'
        _boxline " $status_line" "$status_style"
        _rule '├' '┤'
        _boxline '' plain
        for ((i=0; i<m; i++)); do
            # hdr[i] = "-" puts a blank line above the item, which is what
            # separates Quit and Back from the choices that do something.
            [ "${hdr[$i]:-}" = "-" ] && _boxline '' plain
            if [ "$i" -eq "$sel" ]; then _boxline "  > ${labels[$i]}" sel
            else _boxline "    ${labels[$i]}" plain; fi
        done
        _boxline '' plain
        _rule '├' '┤'
        _boxline " ${helps[$sel]}" dim
        _boxline " Up/Down  move     Enter  choose     q  ${qhint}" dim
        _rule '╰' '╯'
        printf '\033[J' >"$tty"          # wipe whatever a longer list left below
    }

    _build_main() {
        view=main
        qhint=quit
        if [ "$INSTALLED" = not-installed ]; then
            status_line="Not installed yet - choose Install or update to get started."
            status_style=dim
        else
            status_line="$INSTALLED"
            status_style=ok
        fi
        ids=(newest pickver m_remove quit)
        labels=(
            "Install or update Tuni  (recommended)"
            "Install an older version"
            "Remove Tuni"
            "Quit"
        )
        helps=(
            "Installs the newest release. Asks for your password."
            "Pick an exact version from the release page."
            "Uninstall Tuni, with its settings or without."
            "Close this installer without changing anything."
        )
        hdr=("" "" "" "-")
    }

    _build_remove() {
        view=remove
        qhint=back
        status_line="Remove Tuni"
        status_style=dim
        ids=(uninstall purge __back)
        labels=(
            "Uninstall Tuni"
            "Uninstall and delete my settings too"
            "Back"
        )
        helps=(
            "Removes the package. Keeps your settings and session."
            "Removes the package, your settings and your session."
            "Return to the main menu."
        )
        hdr=("" "" "-")
    }

    _build_versions() {
        view=versions
        qhint=back
        status_line="Choose a version to install (newest first)"
        status_style=dim
        ids=(); labels=(); helps=(); hdr=()
        local t
        for t in "${VER_TAGS[@]}"; do
            ids+=("$t"); labels+=("Tuni ${t#v}"); helps+=("Install version ${t#v}."); hdr+=("")
        done
        ids+=("__back"); labels+=("Back"); helps+=("Go back without installing."); hdr+=("-")
    }

    _go_back() {
        case "$view" in
            versions|remove) _build_main; sel=0 ;;
            *)               return 1 ;;
        esac
    }

    # Fills VER_TAGS with at most 10 tags, newest first; 1 when the page could
    # not be read at all.
    _load_versions() {
        local json t count=0
        json="$(fetch "${API_BASE}/releases?per_page=30" 2>/dev/null)" || return 1
        VER_TAGS=()
        while IFS= read -r t; do
            [ -n "$t" ] || continue
            count=$(( count + 1 )); [ "$count" -le 10 ] || break
            VER_TAGS+=("$t")
        done <<EOF
$(printf '%s' "$json" | grep -oE '"tag_name": *"[^"]+"' | sed -E 's/.*"([^"]+)".*/\1/')
EOF
        [ "${#VER_TAGS[@]}" -gt 0 ]
    }

    INSTALLED="$(installed_status)"
    _build_main
    _draw
    while :; do
        IFS= read -rsn1 -u 3 key || key=q
        n=${#ids[@]}
        act=none
        case "$key" in
            $'\033')
                read -rsn2 -t 0.05 -u 3 rest || rest=""
                case "$rest" in '[A') act=up ;; '[B') act=down ;; '') act=cancel ;; esac ;;
            k|K) act=up ;;
            j|J) act=down ;;
            q|Q) act=cancel ;;
            '' | $'\n' | $'\r') act=enter ;;
        esac
        case "$act" in
            up)   sel=$(( (sel - 1 + n) % n )) ;;
            down) sel=$(( (sel + 1) % n )) ;;
            cancel)
                if _go_back; then :; else MENU_CHOICE=quit; break; fi ;;
            enter)
                id="${ids[$sel]}"
                case "$id" in
                    m_remove) _build_remove; sel=0 ;;
                    __back)   _go_back || true ;;
                    pickver)
                        status_line="Loading versions..."; status_style=dim
                        ids=(loading); labels=("Please wait...")
                        helps=("Reading the release page."); hdr=(""); sel=0
                        _draw
                        if _load_versions; then _build_versions; sel=0
                        else _build_main; sel=0
                             status_line="Could not read the release page - check your internet."
                             status_style=dim; fi ;;
                    *)
                        if [ "$view" = versions ]; then
                            REQ_TAG="$id"; MENU_CHOICE=newest
                        else
                            MENU_CHOICE="$id"
                        fi
                        break ;;
                esac ;;
        esac
        _draw
    done

    trap - INT
    stty "$saved" <&3 2>/dev/null || true
    printf '\033[?25h' >"$tty"          # cursor back; stay in the alt screen for the install
    exec 3<&-
}

# --- removing -----------------------------------------------------------
# Purge takes the config and the saved session, which is the one thing here
# that cannot be undone, so it is confirmed on the normal screen where the
# question survives long enough to be read.
do_uninstall() {
    local v answer
    v="$(installed_version || true)"
    if [ -z "$v" ]; then
        leave_alt
        if flatpak_installed; then
            warn "Tuni here is a Flatpak. Remove it with: flatpak uninstall ${FLATPAK_ID}"
            return 1
        fi
        warn "Tuni does not seem to be installed - nothing to remove."
        return 1
    fi
    if [ "$PURGE" -eq 1 ]; then
        leave_alt
        printf 'This deletes Tuni %s, your ~/.config/tuni and your saved session.\n' "$v"
        printf 'Type y to confirm: '
        read -r answer </dev/tty || answer=""
        case "$answer" in
            [yY]*) : ;;
            *) say "Nothing was changed."; return 0 ;;
        esac
    fi
    say "Removing Tuni ${v} (this asks for your password)"
    case "$manager" in
        dnf)    priv dnf remove -y tuni ;;
        apt)    priv apt-get purge -y tuni ;;
        pacman) priv pacman -R --noconfirm tuni ;;
        *)      die "No package manager here knows how to remove Tuni." ;;
    esac
    if [ "$PURGE" -eq 1 ]; then
        rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/tuni" \
               "${XDG_DATA_HOME:-$HOME/.local/share}/tuni"
        say "Settings and session deleted."
    fi
    leave_alt
    say "Tuni has been removed."
}

# --- installing ---------------------------------------------------------
# One package file, whichever direction it moves the version in. An older
# package is not something a plain install accepts on rpm or deb, and refusing
# with the manager's own wording would be a poor answer to a version the user
# picked from a list on purpose.
install_file() {
    local file="$1" want="$2" cur="$3"
    case "$manager" in
        dnf)
            if [ -n "$cur" ] && older_than "$want" "$cur"; then priv dnf downgrade -y "$file"
            elif [ "$want" = "$cur" ]; then priv dnf reinstall -y "$file"
            else priv dnf install -y "$file"; fi ;;
        apt)    priv apt-get install -y --allow-downgrades "$file" ;;
        pacman) priv pacman -U --noconfirm "$file" ;;
    esac
}

do_install() {
    local release latest current url pattern file arch answer
    if [ -n "$REQ_TAG" ]; then
        release="$(fetch "${API_BASE}/releases/tags/${REQ_TAG}")" \
            || die "Couldn't read release ${REQ_TAG} from the release page."
    else
        release="$(fetch "$API")" \
            || die "Couldn't reach the release page. Check the network and try again."
    fi
    latest="$(tag_of "$release")"
    [ -n "$latest" ] || die "The release page had no version in it."
    current="$(installed_version || true)"

    if [ "$CHECK_ONLY" -eq 1 ]; then
        if [ -z "$current" ]; then
            say "Tuni is not installed here. Newest release: ${latest}."
            exit 10
        fi
        say "Installed: ${current}. Newest release: ${latest}."
        older_than "$current" "$latest" && exit 10
        exit 0
    fi

    # Only the automatic path stops here. From the menu, "install 1.2.0 again"
    # is a repair, and refusing it would be an odd thing for a menu to do.
    if [ "$ASSUME_YES" -eq 1 ] && [ -n "$current" ] && ! older_than "$current" "$latest"; then
        say "Tuni ${current} is already the newest version."
        exit 0
    fi

    if flatpak_installed && [ -z "$current" ]; then
        leave_alt
        warn "Tuni here is a Flatpak, and a sandboxed app cannot install packages on the host.
    Rebuild it from packaging/dev.unisic.Tuni.yml to move to ${latest}."
        exit 0
    fi

    [ -n "$manager" ] || die "There is no ready-made Tuni package for ${ID:-this system} (only Fedora,
    Debian/Ubuntu and Arch are built). Build it from source: https://github.com/${REPO}"

    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64) : ;;
        *) die "The releases are built for x86_64 only; this machine is ${arch}.
    Build it from source: https://github.com/${REPO}" ;;
    esac

    # The menu already asked, by being a menu. This is for the plain
    # `install.sh` run in a shell without one.
    if [ "$ASSUME_YES" -eq 0 ] && [ "$IN_ALT" -eq 0 ]; then
        if [ -n "$current" ]; then say "Tuni ${current} is installed; ${latest} is out."
        else say "Tuni ${latest} is out, and nothing is installed here yet."; fi
        printf '    Install it now? [Y/n] '
        read -r answer </dev/tty || answer=""
        case "$answer" in
            [nN]*) say "Nothing was changed."; exit 0 ;;
        esac
    fi

    case "$manager" in
        # Anchored on "tuni-<digit>" so a debuginfo package on the release page
        # can never be what head -n1 picks.
        dnf)    pattern='tuni-[0-9][^/]*\.rpm$' ;;
        apt)    pattern='tuni_[0-9][^/]*\.deb$' ;;
        pacman) pattern='tuni-[0-9][^/]*\.pkg\.tar\.zst$' ;;
    esac
    url="$(printf '%s' "$release" | asset_url "$pattern")"
    [ -n "$url" ] || die "Release ${latest} carries no package for ${manager}."

    tmpdir="$(mktemp -d)"
    file="${tmpdir}/$(basename "$url")"
    download "$url" "$file"

    say "Installing Tuni ${latest} (this asks for your password)"
    # Each manager refuses a package whose dependencies are not met before it
    # unpacks anything, so a refusal costs the download and leaves the copy
    # that is running exactly where it was.
    install_file "$file" "$latest" "$current"

    leave_alt
    say "Tuni ${latest} is installed. Windows already open keep running the old one until they are restarted."
}

# --- run ----------------------------------------------------------------
# The flags are the whole non-interactive surface; anything else opens the
# menu, and without a terminal there is no menu to open.
if [ "$ASSUME_YES" -eq 0 ] && [ "$CHECK_ONLY" -eq 0 ]; then
    if ! { : >/dev/tty; } 2>/dev/null; then
        die "The Tuni installer is interactive - run it inside a terminal window, or pass -y."
    fi
    enter_alt
    tui_run
    case "$MENU_CHOICE" in
        quit|"")   leave_alt; say "No changes made."; exit 0 ;;
        newest)    ACTION=install ;;
        uninstall) ACTION=uninstall ;;
        purge)     ACTION=uninstall; PURGE=1 ;;
        *)         die "invalid choice" ;;
    esac
fi

case "$ACTION" in
    install)   do_install ;;
    uninstall) do_uninstall ;;
esac
