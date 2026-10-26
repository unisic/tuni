#!/usr/bin/env bash
# Point the two AUR recipes at a release, regenerate .SRCINFO, optionally
# build-test them and push them to the AUR.
#
#   ./sync.sh                     # retarget both recipes at the latest release
#   ./sync.sh --version 1.7.0     # ... at a given one
#   ./sync.sh --check             # also build both in an Arch container
#   ./sync.sh --check bin         # ... only tuni-bin (seconds, no compile)
#   ./sync.sh --push              # also push to ssh://aur@aur.archlinux.org
#
# .SRCINFO is what the AUR web interface reads, and a stale one publishes a
# package nobody can find or install. It is never written by hand here:
# makepkg --printsrcinfo generates it, in an Arch container when this machine
# is not Arch.
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo='unisic/tuni'
version=''
do_check=0
check_scope='all'
do_push=0
image='docker.io/library/archlinux:base-devel'

while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="${2:?--version needs a value}"; shift 2 ;;
        --check)
            do_check=1; shift
            case "${1:-}" in bin|all) check_scope="$1"; shift ;; esac ;;
        --push) do_push=1; shift ;;
        -h|--help) sed -n '2,9p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

say() { printf '==> %s\n' "$*"; }
die() { printf '==> error: %s\n' "$*" >&2; exit 1; }

# The API answers 60 requests an hour per address without a token, and a
# GitHub runner shares its address with other jobs, so the limit can be spent
# before this script asks anything. With GITHUB_TOKEN set, which the workflow
# does, the requests count against the token instead.
api() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        curl -fsSL -H "Authorization: Bearer ${GITHUB_TOKEN}" "$@"
    else
        curl -fsSL "$@"
    fi
}

runtime=''
for c in podman docker; do command -v "$c" >/dev/null 2>&1 && { runtime="$c"; break; }; done

if [ -z "$version" ]; then
    version="$(api "https://api.github.com/repos/${repo}/releases/latest" \
               | sed -n 's/.*"tag_name" *: *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n1)"
    [ -n "$version" ] || die "Could not read the latest release tag."
fi
say "Target version: ${version}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Downloads $1 to $tmp/$2 and prints its sha256. The AUR wants a real
# checksum: SKIP is only for VCS sources.
fetch_sha() {
    curl -fsSL -o "${tmp}/$2" "$1" || die "Download failed: $1"
    sha256sum "${tmp}/$2" | cut -d' ' -f1
}

src_sha="$(fetch_sha "https://github.com/${repo}/archive/refs/tags/v${version}.tar.gz" source.tar.gz)"

# The asset's file name carries the upstream pkgrel, which moves on its own,
# so it is read rather than assumed to be 1.
asset="$(api "https://api.github.com/repos/${repo}/releases/tags/v${version}" \
         | sed -n 's/.*"browser_download_url" *: *"\([^"]*\.pkg\.tar\.zst\)".*/\1/p' | head -n1)"
[ -n "$asset" ] || die "Release v${version} has no .pkg.tar.zst asset."
asset_name="$(basename "$asset")"
upstream_pkgrel="$(printf '%s' "$asset_name" \
    | sed -n "s/^tuni-${version}-\([0-9]\{1,\}\)-x86_64\.pkg\.tar\.zst$/\1/p")"
[ -n "$upstream_pkgrel" ] || die "Could not read a pkgrel out of ${asset_name}."
bin_sha="$(fetch_sha "$asset" "$asset_name")"

# The released package records the dependencies packaging/arch/PKGBUILD gave
# it. A recipe here that lacks one installs and then fails to start, so a
# missing entry is fatal. One way only: an extra entry here is a namcap
# finding the upstream recipe has not caught up with, not a mistake.
released_deps="$(tar --zstd -xOf "${tmp}/${asset_name}" .PKGINFO | sed -n 's/^depend = //p' | sort -u)"
for recipe in tuni tuni-bin; do
    recipe_deps="$(sed -n '/^depends=(/,/)/p' "${here}/${recipe}/PKGBUILD" \
                   | tr -d "()'" | sed 's/^depends=//' | tr ' ' '\n' | sed '/^$/d' | sort -u)"
    missing="$(comm -13 <(printf '%s\n' "$recipe_deps") <(printf '%s\n' "$released_deps"))"
    [ -z "$missing" ] || die "${recipe}/PKGBUILD lacks what the release depends on: $(echo $missing)"
done

bump() { # $1 = PKGBUILD, $2 = sha256 of its first source
    sed -i \
        -e "s/^pkgver=.*/pkgver=${version}/" \
        -e "s/^pkgrel=.*/pkgrel=1/" \
        -e "s/^_pkgrel=.*/_pkgrel=${upstream_pkgrel}/" \
        -e "s/^sha256sums=.*/sha256sums=('$2')/" \
        "$1"
}
bump "${here}/tuni/PKGBUILD" "$src_sha"
bump "${here}/tuni-bin/PKGBUILD" "$bin_sha"
say "Recipes retargeted at ${version} (upstream pkgrel ${upstream_pkgrel})"

# Runs $2 as an unprivileged user over a copy of recipe $1, in an Arch
# container unless this machine has makepkg. The copy is staged in $tmp
# because the checkout may sit on a filesystem the container cannot chown
# (this one is on exFAT), and makepkg refuses to run as root.
in_arch() {
    local stage="${tmp}/stage-$1"
    rm -rf "$stage" && mkdir -p "$stage"
    cp "${here}/$1/PKGBUILD" "$stage/"
    if command -v makepkg >/dev/null 2>&1; then
        [ "$(id -u)" -ne 0 ] || die "makepkg refuses to run as root; use a user with passwordless sudo."
        (cd "$stage" && bash -c "$2")
    elif [ -n "$runtime" ]; then
        # The chown back to container root, which rootless podman maps to the
        # invoking user, runs even when makepkg fails: otherwise the files are
        # owned by a subuid and the EXIT trap cannot delete them.
        "$runtime" run --rm -v "${stage}:/pkg:z" -w /pkg "$image" bash -c '
            set -e
            trap "chown -R 0:0 /pkg" EXIT
            # Only a build needs a current system; .SRCINFO is read from the
            # recipe by the image as it is.
            case "$0" in *"makepkg -s"*)
                pacman -Syu --noconfirm --needed archlinux-keyring base-devel namcap git >/dev/null ;;
            esac
            useradd -m b && chown -R b /pkg
            printf "b ALL=(ALL) NOPASSWD: ALL\n" > /etc/sudoers.d/b
            runuser -u b -- bash -c "$0"' "$2"
    else
        die "Need makepkg, podman or docker."
    fi
}

for pkg in tuni tuni-bin; do
    in_arch "$pkg" 'makepkg --printsrcinfo > .SRCINFO'
    cp "${tmp}/stage-${pkg}/.SRCINFO" "${here}/${pkg}/.SRCINFO"
done
say "Generated .SRCINFO"

if [ "$do_check" -eq 1 ]; then
    # `bin` is for CI: the release workflow's arch job has already compiled
    # and installed this tree, and building it again through the source recipe
    # costs ten minutes to learn nothing new.
    targets='tuni-bin tuni'
    [ "$check_scope" = bin ] && targets='tuni-bin'
    for pkg in $targets; do
        say "Build-testing ${pkg}"
        in_arch "$pkg" 'makepkg -s --noconfirm --cleanbuild && { namcap ./*.pkg.tar.zst || true; }' \
            || die "${pkg} failed to build."
    done
fi

# The AUR takes its git service down for maintenance without notice, and every
# clone and push then fails with git's "Could not read from remote repository",
# the same words a wrong key produces. A retry costs nothing while the service
# is up, and a wrong key still fails every attempt, so nothing is hidden.
# The locals carry a prefix because bash scopes them dynamically and the
# retried command would otherwise see them.
retry() {
    local _max="$1" _delay=15 _n=1
    shift
    until "$@"; do
        [ "$_n" -ge "$_max" ] && return 1
        say "Attempt ${_n}/${_max} failed, retrying in ${_delay}s"
        sleep "$_delay"
        _n=$((_n + 1)) _delay=$((_delay * 2))
    done
}

clone_aur() { rm -rf "$2" && git clone "ssh://aur@aur.archlinux.org/$1.git" "$2"; }

if [ "$do_push" -eq 1 ]; then
    for pkg in tuni tuni-bin; do
        work="${tmp}/aur-${pkg}"
        retry 4 clone_aur "$pkg" "$work" \
            || die "Could not clone ${pkg}.git; git's error is above. 'Permission denied (publickey)' means AUR_SSH_KEY is not a key on the AUR account."
        cp "${here}/${pkg}/PKGBUILD" "${here}/${pkg}/.SRCINFO" "$work/"
        git -C "$work" add PKGBUILD .SRCINFO
        if git -C "$work" diff --cached --quiet; then
            say "${pkg}: already up to date"
            continue
        fi
        git -C "$work" commit -m "Update to ${version}"
        retry 3 git -C "$work" push origin HEAD:master || die "Could not push ${pkg}."
        say "Pushed ${pkg} ${version}"
    done
fi
