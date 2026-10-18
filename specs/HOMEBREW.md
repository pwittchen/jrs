# jrs — Publishing to Homebrew

How to make jrs installable with

```sh
brew install pwittchen/tap/jrs
```

Status: **done, for 0.14.6.** [`pwittchen/homebrew-tap`](https://github.com/pwittchen/homebrew-tap)
serves `Formula/jrs.rb`, which passes `brew style`, `brew audit --strict`,
`brew install` and `brew test`, and the docs list the install line. Each
release's formula is still copied into the tap by hand (§4); automating it
and homebrew-core (§5) are open.

## 1. What exists

The `release` job in `.github/workflows/rust.yml` runs
`packaging/manifests.sh`, which writes `homebrew/jrs.rb` from the release's
`SHA256SUMS` and keeps it in the `package-manifests` artifact. The formula
installs the prebuilt binaries (macOS and Linux musl, ARM64 and x86_64), by
tag, with their checksums; installs the shell completions through
`jrs completions`; prints a caveat about the JDK; and tests `jrs --version`.

## 2. The tap

A tap is a GitHub repository named `homebrew-<name>`; `brew install
<user>/<name>/<formula>` reads `Formula/<formula>.rb` from it. A tap named
`tap` gives the install line above, and can hold the formulae of other
projects later.

1. Create the repository:
   ```sh
   gh repo create pwittchen/homebrew-tap --public --description "Homebrew formulae"
   ```
2. Take the formula of the latest release, from its run's artifact:
   ```sh
   gh run list --workflow rust.yml --branch v<version> -L 1
   gh run download <run-id> -n package-manifests -D /tmp/jrs-manifests
   ```
   or by running the script on the release's checksums:
   ```sh
   gh release download v<version> -p SHA256SUMS -D /tmp/jrs-sums
   packaging/manifests.sh <version> /tmp/jrs-sums/SHA256SUMS /tmp/jrs-manifests
   ```
3. Commit it as `Formula/jrs.rb`, after checking it locally:
   ```sh
   git clone git@github.com:pwittchen/homebrew-tap.git && cd homebrew-tap
   mkdir -p Formula && cp /tmp/jrs-manifests/homebrew/jrs.rb Formula/
   brew style Formula/jrs.rb
   brew install --build-from-source ./Formula/jrs.rb && brew test jrs
   git add Formula/jrs.rb && git commit -m "jrs <version>" && git push
   ```

## 3. Changes in this repository

- `packaging/manifests.sh` and the comment on the `package-manifests` step in
  `rust.yml` name the tap `pwittchen/homebrew-jrs`; rename it to
  `pwittchen/homebrew-tap`.
- Document `brew install pwittchen/tap/jrs` in `README.md`, `DOCS.md` and
  `website/docs/index.html`, beside `install.sh`, Scoop and winget.

## 4. Each release

Copy the new `homebrew/jrs.rb` from the release's `package-manifests`
artifact over `Formula/jrs.rb` in the tap, and commit it.

Optionally automate it: a step at the end of `release` that clones the tap,
copies the formula and pushes, with a fine-grained token that can write to
`homebrew-tap` only, kept as a repository secret. Publishing is a manual
step today on purpose, so this is a decision, not a fix.

## 5. Later: homebrew-core

`brew install jrs`, without a tap, needs a formula in `Homebrew/homebrew-core`.
That one builds from the source tarball (`depends_on "rust" => :build`,
`cargo install`), not from the prebuilt binaries, and the project has to meet
homebrew-core's notability bar (stars, forks, users) first. The tap does not
wait on anyone's review.
