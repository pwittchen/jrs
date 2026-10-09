#!/bin/sh
# Writes the package manager manifests for one jrs release, from the
# release's SHA256SUMS:
#
#   packaging/manifests.sh <version> <SHA256SUMS> <out-dir>
#
# into <out-dir>:
#
#   homebrew/jrs.rb             the formula for a tap (pwittchen/homebrew-jrs)
#   scoop/jrs.json              the manifest for a Scoop bucket
#   winget/pwittchen.jrs*.yaml  the three files of a winget-pkgs manifest
#
# Each points at the release's own assets, by tag, with their checksums. The
# `release` job in .github/workflows/rust.yml runs it and keeps the result as
# the `package-manifests` artifact; publishing them to the tap, the bucket and
# winget-pkgs stays a step the maintainer takes.

set -eu

[ $# -eq 3 ] || { echo "usage: manifests.sh <version> <SHA256SUMS> <out-dir>" >&2; exit 2; }
version="$1"
sums="$2"
out="$3"

REPO="pwittchen/jrs"
DOWNLOAD="https://github.com/$REPO/releases/download/v$version"
DESCRIPTION="A Java build system, in Rust"
HOMEPAGE="https://getjrs.dev"

# The checksum SHA256SUMS lists for an asset.
sha() {
    sum="$(awk -v f="$1" '{ n = $2; sub(/^\*/, "", n) } n == f { print $1 }' "$sums")"
    [ -n "$sum" ] || { echo "error: $sums lists no $1" >&2; exit 1; }
    printf '%s' "$sum"
}

mkdir -p "$out/homebrew" "$out/scoop" "$out/winget"

cat > "$out/homebrew/jrs.rb" <<EOF
class Jrs < Formula
  desc "Java build system written in Rust"
  homepage "$HOMEPAGE"
  version "$version"
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

cat > "$out/scoop/jrs.json" <<EOF
{
  "version": "$version",
  "description": "$DESCRIPTION",
  "homepage": "$HOMEPAGE",
  "license": "Apache-2.0",
  "notes": "jrs needs a JDK 17 or newer on PATH or at JAVA_HOME.",
  "architecture": {
    "64bit": {
      "url": "$DOWNLOAD/jrs-x86_64-pc-windows-msvc.zip",
      "hash": "$(sha jrs-x86_64-pc-windows-msvc.zip)"
    },
    "arm64": {
      "url": "$DOWNLOAD/jrs-aarch64-pc-windows-msvc.zip",
      "hash": "$(sha jrs-aarch64-pc-windows-msvc.zip)"
    }
  },
  "bin": "jrs.exe",
  "checkver": "github",
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "https://github.com/$REPO/releases/download/v\$version/jrs-x86_64-pc-windows-msvc.zip"
      },
      "arm64": {
        "url": "https://github.com/$REPO/releases/download/v\$version/jrs-aarch64-pc-windows-msvc.zip"
      }
    },
    "hash": {
      "url": "\$baseurl/SHA256SUMS"
    }
  }
}
EOF

id="pwittchen.jrs"
schema="1.6.0"
cat > "$out/winget/$id.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $schema
EOF

cat > "$out/winget/$id.installer.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: jrs.exe
  PortableCommandAlias: jrs
Installers:
- Architecture: x64
  InstallerUrl: $DOWNLOAD/jrs-x86_64-pc-windows-msvc.zip
  InstallerSha256: $(sha jrs-x86_64-pc-windows-msvc.zip | tr '[:lower:]' '[:upper:]')
- Architecture: arm64
  InstallerUrl: $DOWNLOAD/jrs-aarch64-pc-windows-msvc.zip
  InstallerSha256: $(sha jrs-aarch64-pc-windows-msvc.zip | tr '[:lower:]' '[:upper:]')
ManifestType: installer
ManifestVersion: $schema
EOF

cat > "$out/winget/$id.locale.en-US.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
PackageLocale: en-US
Publisher: Piotr Wittchen
PublisherUrl: https://github.com/pwittchen
PackageName: jrs
PackageUrl: $HOMEPAGE
License: Apache-2.0
LicenseUrl: https://github.com/$REPO/blob/master/LICENSE
ShortDescription: $DESCRIPTION
Description: jrs builds, tests, runs and packages a Java project from one jrs.toml manifest, resolving dependencies from Maven Central. It needs a JDK 17 or newer.
Moniker: jrs
Tags:
- java
- build-tool
- maven
ReleaseNotesUrl: https://github.com/$REPO/releases/tag/v$version
ManifestType: defaultLocale
ManifestVersion: $schema
EOF

echo "wrote the manifests for jrs $version into $out"
