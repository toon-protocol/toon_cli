#!/bin/sh
# Install `toon` from a GitHub release:
#
#   curl -fsSL https://raw.githubusercontent.com/toon-protocol/toon_cli/main/install.sh | sh
#
# It downloads the release for this machine, checks it against the release's SHA256SUMS,
# and puts `toon` in ~/.local/bin. Two variables change that:
#
#   TOON_VERSION      the release to install, such as v0.1.0 (default: the latest)
#   TOON_INSTALL_DIR  where to put `toon` (default: ~/.local/bin)
#
# Everything is in `main`, which runs only once the whole script has been read, so a
# download cut short does nothing.

set -eu

repo=toon-protocol/toon_cli

fail() {
    echo "toon install: $*" >&2
    exit 1
}

# The release binaries are built on Ubuntu 22.04 (`.github/workflows/release.yml`).
check_glibc() {
    glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}') ||
        glibc=
    [ -n "$glibc" ] ||
        fail "the release binaries need glibc 2.35 or later, and this machine has no glibc. Build from source: cargo install --locked --git https://github.com/$repo"
    if ! printf '%s\n' "$glibc" | awk -F. '{ exit !($1 > 2 || ($1 == 2 && $2 >= 35)) }'; then
        fail "the release binaries need glibc 2.35 or later, and this machine has $glibc. Build from source: cargo install --locked --git https://github.com/$repo"
    fi
}

latest_version() {
    # /releases/latest redirects to /releases/tag/<tag>, or to /releases if there is none.
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") ||
        fail "could not reach github.com to find the latest release"
    echo "${url##*/}"
}

main() {
    [ "$(uname -s)" = Linux ] ||
        fail "release binaries are built for Linux only. Build from source: cargo install --locked --git https://github.com/$repo"
    case $(uname -m) in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "there is no release binary for $(uname -m), only x86_64 and aarch64" ;;
    esac
    for tool in curl tar sha256sum awk getconf; do
        command -v "$tool" >/dev/null 2>&1 || fail "this script needs \`$tool\`"
    done
    check_glibc

    version=${TOON_VERSION:-$(latest_version)}
    case $version in
        v*) ;;
        [0-9]*) version=v$version ;;
        *) fail "found no release of $repo" ;;
    esac
    dir=${TOON_INSTALL_DIR:-$HOME/.local/bin}
    name=toon-$version-linux-$arch
    base=https://github.com/$repo/releases/download/$version

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    echo "Downloading toon $version for linux-$arch"
    curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" ||
        fail "could not download $base/$name.tar.gz: is $version a release?"
    curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" ||
        fail "could not download $base/SHA256SUMS"

    # Check the one line for this archive, so that a SHA256SUMS without it fails.
    grep "  $name.tar.gz\$" "$tmp/SHA256SUMS" >"$tmp/sum" ||
        fail "SHA256SUMS of $version has no checksum for $name.tar.gz"
    (cd "$tmp" && sha256sum --check --status sum) ||
        fail "$name.tar.gz does not match its checksum in SHA256SUMS: not installed"
    tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

    # Copy beside the old binary, then rename over it: a running `toon` keeps its own copy.
    mkdir -p "$dir"
    cp "$tmp/$name/toon" "$dir/.toon.new"
    chmod 755 "$dir/.toon.new"
    mv -f "$dir/.toon.new" "$dir/toon"

    echo "Installed $("$dir/toon" --version) at $dir/toon"
    case ":$PATH:" in
        *":$dir:"*) ;;
        *) echo "$dir is not on your PATH: add it, or run $dir/toon" ;;
    esac
    echo "Next: https://github.com/$repo/blob/main/docs/guide/first-agent-node.md"
    echo "For an agent harness, install the skills that teach it toon: toon skill install"
}

main "$@"
