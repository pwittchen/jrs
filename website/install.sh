#!/bin/sh
# Installs a prebuilt jrs binary from the GitHub releases:
#
#   curl -fsSL https://getjrs.dev/install.sh | sh
#
# It picks the build for this machine, verifies it against the release's
# SHA256SUMS and puts `jrs` in ~/.local/bin. Options go after `sh -s --`:
#
#   curl -fsSL https://getjrs.dev/install.sh | sh -s -- --version 0.5.0 --to /usr/local/bin
#
# or come from the environment as JRS_INSTALL_VERSION and JRS_INSTALL_DIR.
# macOS and Linux only; Windows instructions are at https://getjrs.dev/#install.
#
# Everything happens inside main(), called on the last line, so a download cut
# off halfway runs nothing.

set -eu

REPO="pwittchen/jrs"
RELEASES="https://github.com/$REPO/releases"

usage() {
    cat <<EOF
Install jrs, a Java build system, from $RELEASES.

usage: install.sh [--version <version>] [--to <dir>]

  --version <version>  the release to install, such as 0.5.0 (default: latest)
  --to <dir>           the directory to install into (default: ~/.local/bin)
  -h, --help           print this help

JRS_INSTALL_VERSION and JRS_INSTALL_DIR set the same from the environment.
EOF
}

say() {
    printf '%s\n' "$*"
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "this script needs \`$1\`, which is not on PATH"
}

main() {
    version="${JRS_INSTALL_VERSION:-latest}"
    dir="${JRS_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}"

    while [ $# -gt 0 ]; do
        case "$1" in
            --version)
                [ $# -ge 2 ] || die "--version needs a value"
                version="$2"
                shift 2
                ;;
            --version=*)
                version="${1#*=}"
                shift
                ;;
            --to)
                [ $# -ge 2 ] || die "--to needs a value"
                dir="$2"
                shift 2
                ;;
            --to=*)
                dir="${1#*=}"
                shift
                ;;
            -h | --help)
                usage
                exit 0
                ;;
            *)
                die "unknown option \`$1\` (see --help)"
                ;;
        esac
    done

    need uname
    need tar
    need mktemp

    target="$(detect_target)" || exit 1
    asset="jrs-$target.tar.gz"
    # Release assets carry no version, so the latest one has a stable URL.
    case "$version" in
        latest) base="$RELEASES/latest/download" ;;
        v*) base="$RELEASES/download/$version" ;;
        *) base="$RELEASES/download/v$version" ;;
    esac

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 130' HUP INT TERM

    say "downloading $asset ($version)"
    download "$base/$asset" "$tmp/$asset" ||
        die "could not download $base/$asset (is \`$version\` a jrs release?)"
    download "$base/SHA256SUMS" "$tmp/SHA256SUMS" ||
        die "could not download $base/SHA256SUMS"
    verify "$tmp" "$asset"

    tar -xzf "$tmp/$asset" -C "$tmp" jrs || die "$asset is not a valid archive"

    # Copied next to its destination and renamed over it, so a binary that is
    # running is replaced rather than written into, and an interrupted install
    # never leaves a half-written one behind.
    mkdir -p "$dir" || die "could not create $dir"
    cp "$tmp/jrs" "$dir/.jrs.$$" || die "could not write to $dir"
    chmod 755 "$dir/.jrs.$$"
    mv -f "$dir/.jrs.$$" "$dir/jrs" || {
        rm -f "$dir/.jrs.$$"
        die "could not write $dir/jrs"
    }

    installed="$("$dir/jrs" --version 2>/dev/null)" ||
        die "installed $dir/jrs, but it does not run on this machine"
    say "installed $installed to $dir/jrs"

    case ":$PATH:" in
        *":$dir:"*) ;;
        *) path_hint "$dir" ;;
    esac

    say "jrs needs a JDK 17 or newer on PATH or at JAVA_HOME; run \`jrs init\` to start a project"
}

# Prints the release target for this machine: the part of the asset name
# after `jrs-`.
detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Darwin)
            # A shell running under Rosetta reports x86_64 on Apple Silicon,
            # where the native build is the one to have.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
                arch=arm64
            fi
            case "$arch" in
                arm64 | aarch64) echo aarch64-apple-darwin ;;
                x86_64) echo x86_64-apple-darwin ;;
                *) die "there is no prebuilt jrs for macOS on $arch; see $RELEASES" ;;
            esac
            ;;
        Linux)
            # The Linux builds link musl statically and run on any distribution.
            case "$arch" in
                x86_64 | amd64) echo x86_64-unknown-linux-musl ;;
                aarch64 | arm64) echo aarch64-unknown-linux-musl ;;
                *) die "there is no prebuilt jrs for Linux on $arch; build it from source, see https://getjrs.dev/#install" ;;
            esac
            ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            die "this script installs jrs on macOS and Linux; for Windows, see https://getjrs.dev/#install"
            ;;
        *)
            die "there is no prebuilt jrs for $os; build it from source, see https://getjrs.dev/#install"
            ;;
    esac
}

download() {
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget --https-only -q -O "$2" "$1"
    else
        die "this script needs \`curl\` or \`wget\`, and neither is on PATH"
    fi
}

# Checks $1/$2 against the checksum $1/SHA256SUMS lists for it.
verify() {
    expected="$(awk -v f="$2" '{ n = $2; sub(/^\*/, "", n) } n == f { print $1 }' "$1/SHA256SUMS")"
    [ -n "$expected" ] || die "SHA256SUMS lists no checksum for $2"

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$1/$2")"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$1/$2")"
    else
        say "warning: neither \`sha256sum\` nor \`shasum\` is on PATH; skipping checksum verification" >&2
        return 0
    fi
    actual="${actual%% *}"

    [ "$actual" = "$expected" ] || die "checksum mismatch for $2: expected $expected, got $actual"
}

# The `~` in the file names is printed for the reader, not expanded.
# shellcheck disable=SC2088
path_hint() {
    case "${SHELL:-}" in
        */zsh) rc="~/.zshrc" ;;
        */bash) rc="~/.bashrc" ;;
        */fish)
            say "$1 is not on your PATH; add it with: fish_add_path $1"
            return
            ;;
        *) rc="your shell's startup file" ;;
    esac
    say "$1 is not on your PATH; add this line to $rc:"
    say "  export PATH=\"$1:\$PATH\""
}

main "$@"
