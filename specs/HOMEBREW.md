# jrs — Publishing to Homebrew

How to make jrs installable with

```sh
brew install pwittchen/tap/jrs
```

Status: **done.** [`pwittchen/homebrew-tap`](https://github.com/pwittchen/homebrew-tap)
serves `Formula/jrs.rb`, which passes `brew style`, `brew audit --strict`,
`brew install` and `brew test`, and the docs list the install line. Each
release updates the tap (§4); homebrew-core (§5) is open.

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
  `website/docs/index.html`, beside `install.sh`.

## 4. Each release

Nothing by hand. The `homebrew` job in `rust.yml` runs after `release`, only
on a `v*` tag and only in `pwittchen/jrs`, and dispatches the tap's **Update
formulae** workflow for the tag's version:

```sh
gh workflow run update-formulae.yml --repo pwittchen/homebrew-tap \
  --ref master -f formula=jrs -f version=<version>
```

That workflow (`scripts/update-formulae.sh` in the tap) writes the formula with
the tag's `manifests.sh`, checks every URL and checksum against the release's
`SHA256SUMS`, runs `brew style`, commits `jrs <version>` and pushes. The tap
stays the one place that writes to itself, whether the run came from a release
or from its Actions tab, and jrs holds no write access to its contents.

The secret is `HOMEBREW_TAP_TOKEN`: a fine-grained token for
`pwittchen/homebrew-tap` alone, with the repository permission **Actions: read
and write**. It expires, and the job fails until it is replaced.

Nothing depends on the job, as [REACH §2.3](REACH.md#23-failure-policy)
asks: the GitHub release, its assets and `install.sh` are out before it starts,
and the website does not wait for it. A failure is a red job to retry, in this
repository if the dispatch failed and in the tap if its run did; the job
returns as soon as the run is dispatched, so a run that fails in the tap shows
red there and not here. A retry changes nothing once the formula is at the
version. The tap's script takes `X.Y.Z` only, so a tag such as `v1.0.0-rc.1`
leaves the formula where it is, and a red run in the tap.

The release run checks less than a person does: `brew audit --strict`,
`brew install` and `brew test` are not part of it. A release changes only the
version and the checksums, so that is enough until `manifests.sh` changes. After
a change to the formula's content, or to backfill a release, use the Claude Code
skill `/update-homebrew-tap [version] [tap checkout]`
([`.claude/skills/update-homebrew-tap`](../.claude/skills/update-homebrew-tap/SKILL.md)).
It defaults to the latest release and `../homebrew-tap`, and writes the formula
from the release's `SHA256SUMS` with the checkout's current `manifests.sh`. It
checks the formula with `brew style`, `brew audit --strict`, `brew install` and
`brew test` from a throwaway local tap, commits `jrs <version>` in the tap, and
pushes only on a yes.

## 5. Later: homebrew-core

`brew install jrs`, without a tap, needs a formula in `Homebrew/homebrew-core`.
That one builds from the source tarball (`depends_on "rust" => :build`,
`cargo install`), not from the prebuilt binaries, and the project has to meet
homebrew-core's notability bar (stars, forks, users) first. The tap does not
wait on anyone's review.
