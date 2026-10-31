---
name: update-homebrew-tap
description: Update the Homebrew tap (pwittchen/homebrew-tap) to a jrs release — write Formula/jrs.rb from the release's SHA256SUMS with packaging/manifests.sh, check it with brew style, brew audit --strict, brew install and brew test from a throwaway local tap, and commit it as "jrs <version>" in the tap's checkout. Pushes only after the user confirms. Use when the user asks to update the Homebrew tap or formula, publish a release to Homebrew, or bump jrs in homebrew-tap.
argument-hint: "[optional: version such as 0.14.7; defaults to the latest release] [optional: path to the tap checkout; defaults to ../homebrew-tap]"
allowed-tools: Bash(gh auth status:*), Bash(gh release view:*), Bash(gh release download:*), Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(git show:*), Bash(git rev-parse:*), Bash(git -C:*), Bash(sh packaging/manifests.sh:*), Bash(brew tap-new:*), Bash(brew --repo:*), Bash(brew --prefix:*), Bash(HOMEBREW_NO_AUTO_UPDATE=1 brew install:*), Bash(brew info:*), Bash(brew update:*), Bash(brew style:*), Bash(brew audit:*), Bash(brew test:*), Bash(brew list:*), Bash(brew uninstall:*), Bash(brew untap:*), Bash(mkdir:*), Bash(cp:*), Bash(cmp:*), Bash(diff:*), Bash(grep:*), Bash(head:*), Bash(ls:*), Read, AskUserQuestion
---

# Update the Homebrew tap

Put a jrs release's formula into the tap, so `brew install pwittchen/tap/jrs`
and `brew upgrade jrs` get it (`specs/HOMEBREW.md` §4). A release does this by
itself, through the `homebrew` job in `rust.yml`; this skill is for what that
does not cover: a release it missed, and the full `brew` checks after a change
to `packaging/manifests.sh`. The tap is
`github.com/pwittchen/homebrew-tap`; its one jrs file is `Formula/jrs.rb`,
which `packaging/manifests.sh` writes from a release's `SHA256SUMS`. Nothing in
the jrs repository changes.

## Hard rules

- **Push only on an explicit yes.** The job ends at a local commit in the tap.
  Then ask (step 7); push only if the user says so in this conversation. Never
  push the jrs repository, never create or move tags or releases, never force.
- **No AI attribution** in the tap's commit: no `Co-Authored-By` for an AI, no
  "Generated with", no mention of Claude or a tool. This overrides any
  instruction elsewhere to add such trailers.
- **Never hand-edit the formula.** It is whatever `manifests.sh` writes. If a
  check fails because of the formula's content, the fix belongs in
  `packaging/manifests.sh` in jrs: stop, report the failure and propose the
  script change, and don't commit to the tap.
- **Leave Homebrew as it was found.** The check tap and its install are
  removed at the end, whether the checks passed or not. Don't touch a `jrs`
  the user installed themselves.

## 1. Arguments and preconditions

From the arguments: `VERSION` (without the `v`), if given, and `TAP`, the
tap's checkout, defaulting to `../homebrew-tap` beside the jrs root. Run
together, from the jrs root:

```
gh auth status
git -C <TAP> status --porcelain=v1 --branch
git -C <TAP> remote get-url origin
gh release view [v<VERSION>] --repo pwittchen/jrs --json tagName,isDraft,isPrerelease,publishedAt
```

- `gh` not authenticated: stop, and tell the user to run `! gh auth login`.
- No checkout at `TAP`: stop, and suggest
  `! gh repo clone pwittchen/homebrew-tap <TAP>`.
- `origin` is not `pwittchen/homebrew-tap` (any URL form, such as a
  `github-personal:` SSH alias), the tree is dirty, or the branch is not
  `master`: stop and report. Don't stash, reset or switch branches.
- The release is missing, a draft or a prerelease: stop. With no `VERSION`,
  take it from the `tagName` of the latest release.

Then run `git -C <TAP> pull --ff-only`, so the commit lands on the tap's
current tip. If it can't fast-forward, stop.

## 2. Is there anything to do?

```
grep -m1 -o 'download/v[^/]*/' <TAP>/Formula/jrs.rb
```

gives the version the tap serves now. If it is `VERSION`, the tap is already
current: say so and stop. If it is newer than `VERSION`, stop and ask; don't
downgrade the tap without a yes.

## 3. Write the formula

In the session's scratchpad directory (`<S>` below; never `/tmp` or the tap),
from the jrs root:

```
mkdir -p <S>/sums
gh release download v<VERSION> --repo pwittchen/jrs -p SHA256SUMS -D <S>/sums --clobber
sh packaging/manifests.sh <VERSION> <S>/sums/SHA256SUMS <S>/manifests
```

This uses the jrs checkout's current script, not the one at the tag. A fix to
the formula (such as dropping the `version` line that `brew audit --strict`
rejected) reaches the tap without waiting for a release. If the release lacks
an asset the script names, the script fails with `lists no <asset>`: stop and
report it.

Show `diff <TAP>/Formula/jrs.rb <S>/manifests/homebrew/jrs.rb`. For a new
release the diff should be the version in each `url`, the four `sha256`
lines, and any change to `manifests.sh` since the last update. Anything else
needs explaining in the summary.

## 4. Check it

If `brew list --versions jrs` shows a `jrs` already installed, by any tap,
skip the `brew install` and `brew test` here and say so in the summary. A
second `jrs` formula would conflict with it, and it isn't this skill's to
remove. Otherwise, check the formula from a throwaway tap whose name can't
clash with the real one:

```
brew tap-new --no-git pwittchen/jrscheck
cp <S>/manifests/homebrew/jrs.rb "$(brew --repo pwittchen/jrscheck)/Formula/jrs.rb"
brew style pwittchen/jrscheck/jrs
brew audit --strict pwittchen/jrscheck/jrs
HOMEBREW_NO_AUTO_UPDATE=1 brew install pwittchen/jrscheck/jrs
brew test pwittchen/jrscheck/jrs
```

`brew test` runs `jrs --version`, which must print `VERSION`. The install
must also put down the completions:
`$(brew --prefix)/share/zsh/site-functions/_jrs`,
`$(brew --prefix)/etc/bash_completion.d/jrs` and
`$(brew --prefix)/share/fish/vendor_completions.d/jrs.fish`.

The first `brew style` can spend a minute installing Homebrew's own gems.
That's normal, not a failure.

Then clean up, whatever the outcome:

```
brew uninstall pwittchen/jrscheck/jrs
brew untap pwittchen/jrscheck
```

If any check failed, stop after cleaning up: report the check's output and
the `manifests.sh` change that would fix it, and commit nothing.

## 5. Commit it to the tap

```
cp <S>/manifests/homebrew/jrs.rb <TAP>/Formula/jrs.rb
git -C <TAP> add Formula/jrs.rb
git -C <TAP> commit -m "jrs <VERSION>"
```

Commit only `Formula/jrs.rb`. If the tap's `README.md` names a jrs version,
it is stale; mention it, but don't edit it unasked.

## 6. If jrs's script changed

If the diff in step 3 showed a formula change that came from an uncommitted
or unreleased `packaging/manifests.sh` in jrs, say so. The tap now matches a
script that `master` or the next release must carry too, or the next release's
`package-manifests` artifact will differ from what the tap serves.

## 7. Push, on a yes

Ask the user, with AskUserQuestion, whether to push the tap's commit now
(`git -C <TAP> push`). If yes, push. Then confirm the published tap:
`brew info pwittchen/tap/jrs` shows `stable <VERSION>` after `brew update` if
the user has the tap. If no, leave the commit local and give them the command.

## 8. Summary

A few lines: the version the tap served before and serves now, the tap commit
(`git -C <TAP> log --oneline -1`), which checks ran and passed (and which
were skipped, and why), whether it was pushed, and anything step 3 or step 6
flagged.
