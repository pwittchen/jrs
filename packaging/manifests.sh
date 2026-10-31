#!/bin/sh
# Writes the Homebrew formula for one jrs release, from the
# release's SHA256SUMS:
#
#   packaging/manifests.sh <version> <SHA256SUMS> <out-dir>
#
# into <out-dir>:
#
#   homebrew/jrs.rb             the formula for the tap (pwittchen/homebrew-tap)
#
# Each points at the release's own assets, by tag, with their checksums. The
# `release` job in .github/workflows/rust.yml runs it and keeps the result as
# the `package-manifests` artifact. The tap runs this script itself, at the
# release's tag, when the `homebrew` job there asks it to.

set -eu

[ $# -eq 3 ] || { echo "usage: manifests.sh <version> <SHA256SUMS> <out-dir>" >&2; exit 2; }
version="$1"
sums="$2"
out="$3"

REPO="pwittchen/jrs"
DOWNLOAD="https://github.com/$REPO/releases/download/v$version"
HOMEPAGE="https://getjrs.dev"

# The checksum SHA256SUMS lists for an asset.
sha() {
    sum="$(awk -v f="$1" '{ n = $2; sub(/^\*/, "", n) } n == f { print $1 }' "$sums")"
    [ -n "$sum" ] || { echo "error: $sums lists no $1" >&2; exit 1; }
    printf '%s' "$sum"
}

mkdir -p "$out/homebrew"

cat > "$out/homebrew/jrs.rb" <<EOF
class Jrs < Formula
  desc "Java build system written in Rust"
  homepage "$HOMEPAGE"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "$DOWNLOAD/jrs-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha jrs-aarch64-apple-darwin.tar.gz)"
    end
    on_intel do
      url "$DOWNLOAD/jrs-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha jrs-x86_64-apple-darwin.tar.gz)"
    end
  end

  on_linux do
    on_arm do
      url "$DOWNLOAD/jrs-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$(sha jrs-aarch64-unknown-linux-musl.tar.gz)"
    end
    on_intel do
      url "$DOWNLOAD/jrs-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$(sha jrs-x86_64-unknown-linux-musl.tar.gz)"
    end
  end

  def install
    bin.install "jrs"
    generate_completions_from_executable(bin/"jrs", "completions")
  end

  def caveats
    <<~TEXT
      jrs drives the JDK's own tools: it needs a JDK 17 or newer on PATH or
      at JAVA_HOME, such as \`brew install openjdk\`.
    TEXT
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/jrs --version")
  end
end
EOF

echo "wrote the formula for jrs $version into $out"
