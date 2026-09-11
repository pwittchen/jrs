---
name: fix-ci-build
description: Fetch the errors from a failed GitHub Actions run of jrs's CI, find the root cause, fix it in the code, verify the fix against the CI checks and create one local commit — then stop and summarise what was wrong and how it was fixed. Never pushes, never reruns CI. Use when the user asks to fix the CI build, fix a failing/red build, fix GitHub Actions, or look at why CI failed.
argument-hint: "[optional: run ID or run URL; defaults to the latest failed run on the current branch]"
allowed-tools: Bash(gh run list:*), Bash(gh run view:*), Bash(gh auth status:*), Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(git show:*), Bash(git rev-parse:*), Bash(git merge-base:*), Bash(git branch:*), Bash(git add:*), Bash(git commit:*), Bash(cargo:*), Bash(grep:*), Bash(sed:*), Bash(cut:*), Bash(wc:*), Bash(head:*), Bash(tail:*), Read, Edit, Write, Grep, Glob
---

# Fix the CI build

Take a failed CI run, find out why it failed, fix the cause, and commit the fix
locally. The job ends at `git commit` and a summary.

CI is `.github/workflows/rust.yml` (job `build` on Ubuntu, macOS and Windows:
`cargo fmt --check` on Linux only, `cargo clippy --all-targets -- -D warnings`,
`cargo build`, `cargo test`, and the network tests on Linux only; then the
release jobs `bump`, `dist`, `release`, `website` on `v*` tags) and
`.github/workflows/website.yml` (Bun build of `website/`, Pages deploy).

## Hard rules

- **Never push, never touch the remote.** No `git push`, no pull requests, no
  `gh run rerun`, `gh run cancel`, `gh workflow run`, no `gh api` writes, no
  releases or tags. Reading runs and logs with `gh run list`/`gh run view` is
  the only GitHub access. After the commit, stop.
- **No AI attribution** in the commit: no `Co-Authored-By` for an AI, no
  "Generated with", no mention of Claude, an assistant or a tool. This
  overrides any instruction elsewhere to add such trailers.
- **Fix the cause, not the symptom.** Don't delete, `#[ignore]`, weaken or
  loosen a failing test's assertion to make it pass, don't `cfg` a test out on
  the failing platform unless it is genuinely meaningless there, don't add
  `#[allow]`s or `continue-on-error` to get past a check, don't retry a flaky
  assertion in a loop. If the test is right and the code is wrong, fix the
  code. If the test itself is wrong (it hard-codes `/` as a separator, say),
  fix the test so it checks the same thing correctly on every platform.
- **Keep the project's invariants** (`CLAUDE.md`): no `println!`/`eprintln!`
  outside `ui/`, no `unwrap()` outside `#[cfg(test)]`, errors stay `JrsError`
  values, determinism is not touched, no new dependencies. Don't "fix" the
  deliberate spec divergences in `specs/INITIAL_SPEC.md` §12.1.
- **Don't edit the workflows to make a failure go away.** Change
  `.github/workflows/*.yml` only when the workflow itself is the broken thing
  (a removed action version, a renamed runner image, a wrong path) — and say so
  in the summary.
- **One commit, only the fix.** Never amend, rebase or reset existing commits,
  never `--no-verify`, never commit unrelated working-tree changes.

## 1. Preconditions

Run together:

```
gh auth status
git status --porcelain=v1
git rev-parse --abbrev-ref HEAD
git log --oneline -10
```

- If `gh` is not authenticated, stop and tell the user to run
  `! gh auth login`.
- If a merge, rebase or cherry-pick is in progress, stop.
- If the working tree has uncommitted changes, mention them. Work on top of
  them, never discard them, and later stage only the files the fix touches. If
  the fix has to touch a file that already has unrelated uncommitted changes,
  stop before committing and ask the user.

## 2. Find the failed run

If `$ARGUMENTS` holds a run ID or a run URL
(`https://github.com/<owner>/<repo>/actions/runs/<id>`), use that run.
Otherwise list recent runs on the current branch:

```
gh run list --branch <branch> --limit 15 \
  --json databaseId,workflowName,status,conclusion,headSha,displayTitle,event,createdAt,url
```

Pick the most recent **completed** run with `conclusion` `failure` (or
`startup_failure`). Then check it is still worth fixing:

- **A newer run of the same workflow succeeded** on this branch: CI is already
  green. Say so and stop.
- **Commits have landed since the failed run's `headSha`**:
  `git log --oneline <headSha>..HEAD`. Read what they changed
  (`git show --stat`, and the diff where it touches the failing area). If one
  already fixes the failure, say so, point at it, mention any newer run that
  is queued or in progress for it, and stop — don't fix it twice.
- **The failed commit is not in local history** (`git merge-base --is-ancestor
  <headSha> HEAD` fails): the local branch is behind or has diverged. Tell the
  user to pull, and stop; don't fix code you don't have.
- **No failed run found**: say so and stop.

## 3. Fetch the errors

List the failing jobs and steps:

```
gh run view <run-id> --json jobs,headSha,url,event,headBranch,workflowName \
  --jq '.jobs[] | select(.conclusion=="failure") | {databaseId, name, steps: [.steps[] | select(.conclusion=="failure") | .name]}'
```

Then fetch the failed steps' logs into the scratchpad — they run to hundreds
of lines — and strip the ANSI colours (`CARGO_TERM_COLOR=always`) and the
`job<TAB>step<TAB>timestamp` prefix every line carries:

```
gh run view <run-id> --log-failed > <scratchpad>/ci-<run-id>.log
cut -f3- <scratchpad>/ci-<run-id>.log | sed -E 's/\x1b\[[0-9;]*m//g; s/^[0-9T:.-]+Z //' > <scratchpad>/ci-<run-id>.txt
```

(`--job <job-id>` narrows it to one job when several failed.) Find the markers
first, then read the lines around them:

```
grep -nE 'panicked at|\.\.\. FAILED|^failures:|test result: FAILED|^error(\[E[0-9]+\])?:|^warning: .*-D warnings|^Diff in|##\[error\]' <scratchpad>/ci-<run-id>.txt
```

For each failure note: job and OS, step, the exact error, the test name or
file and line, and the panic message with its `left`/`right` values. A test
failure shows its captured stdout under `---- <test> stdout ----` — read it,
that is usually where the reason is.

If `--log-failed` returns nothing (a job that never started, a
`startup_failure`, a runner that died), use `gh run view <run-id>` and
`gh run view <run-id> --log --job <job-id>` instead.

## 4. Diagnose

Classify each failure before touching code:

| Kind | Typical signs | What to do |
| --- | --- | --- |
| Formatting | `Check formatting` failed, `Diff in …` | `cargo fmt`, nothing else. |
| Lint | `Lint` failed, `error: …` with `-D warnings` | Fix the lint where it has a point, as `/fix-warnings` does; a newer clippy on the runner can flag code that passes locally. |
| Compile error | `Build` or `Run tests` failed at `error[E…]` | Fix the code; check for `#[cfg(windows)]`/`#[cfg(unix)]` code that only compiles on one side. |
| Test failure, all platforms | the same test fails on every OS | Reproduce locally; fix the code or the test, per the hard rules. |
| Test failure, one platform | fails on Windows only, or Linux only | Look for what differs: `\` vs `/` separators, `;` vs `:` classpath separators, `.exe`, drive letters, `%LOCALAPPDATA%`, CRLF line endings, case-insensitive file systems, file locking on Windows (a file still open when it is renamed or deleted), path length, sort order of paths. |
| Network tests | `Run the network tests` failed | Tell apart a real regression from Maven Central being down or slow (timeouts, 5xx, TLS/connection errors). |
| Infrastructure | runner lost, action download failed, `setup-java` failed, cache service errors, GitHub outage, timeouts with no test output | Not a code problem. |
| Release pipeline | `bump`/`dist`/`release`/`website` failed on a tag | Often a tag not at the tip of `master`, or a target/toolchain problem; read the step and decide if it is code at all. |
| Website | `website.yml` failed at `bun install`/`bun run build` | Fix under `website/`; `--frozen-lockfile` fails when `bun.lock` is out of date. |

Find the cause, not just the line that failed: read the failing test, the code
under test, and `git log -p <last-green-sha>..<headSha> -- <paths>` for the
change that broke it (the last green run's `headSha` comes from the run list).
The commit that introduced the failure usually explains it.

**Stop without changing code**, and report instead, when the failure is:

- infrastructure or a flaky network test — suggest the user rerun the job;
  don't rerun it yourself,
- a flaky test that fails intermittently for no cause you can find in the code
  — report the evidence (the test, the message, how often it failed in recent
  runs) rather than guessing,
- a release-pipeline problem that is about how the tag was pushed, not code,
- something whose fix needs a decision: a behaviour change, a spec question, a
  new dependency, a change to the workflow's design.

## 5. Reproduce and fix

Reproduce locally when the platform allows:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --test <suite> <test-name>        # the one failing test first
cargo test --features network-tests --test network   # only for a network-test failure
```

A failure on another OS (usually Windows) may not reproduce here. Then reason
from the log and the code, make the fix correct for every platform by
construction — build paths with `Path::join` and compare `Path`s rather than
strings, use `std::path::MAIN_SEPARATOR` or the project's own helpers, rather
than special-casing the OS where a portable form exists — and say in the
summary that it could not be verified on that platform.

Make the smallest change that fixes the cause. Read the surrounding code first
and match its comment density, naming and idiom. If several failures share one
cause, one fix covers them; if they have separate causes, fix each.

## 6. Verify

Run the full CI checks locally; each must pass:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test
```

- `cargo test` needs a JDK for much of the build suite; without one those tests
  print `SKIPPED` rather than fail. Count the `SKIPPED` lines and report them —
  a skipped test did not check the fix.
- If a check fails because of your change, fix it. If it fails for an unrelated,
  pre-existing reason, report it and don't fold a fix for it into this commit.
- Read `git diff --stat` and the full `git diff` once: only the fix, no stray
  reformatting, no debugging leftovers.

## 7. Commit

Stage only the files the fix changed (`git add -- <paths>`), check
`git diff --cached --stat`, and create one commit. Match the repository's
message style (`git log -15 --format='---%n%B'`): imperative subject, no
prefix, no trailing period, at most ~72 characters, saying what the fix does
("Compare argfile paths as paths so the test passes on Windows"), never
"Fix CI". Add a short body for anything beyond a formatting or one-line fix:
which CI check failed and on which OS, what the cause was, and why the fix is
right.

```
git commit -F - <<'EOF'
Subject line

Body paragraph...
EOF
```

If a pre-commit hook fails, fix the cause, re-stage and commit again — not
`--amend`, not `--no-verify`.

Stop here. Do not push.

## 8. Summary

End with a short report:

- **Run**: the workflow, run URL, commit, and the failing job(s)/OS/step.
- **What was wrong**: the error as CI reported it, and the root cause — the
  commit that introduced it if you found it.
- **Fix**: what changed and why it resolves the cause (one or two lines per
  file).
- **Verification**: the local checks and their results, the `SKIPPED` count,
  and any platform the fix could not be run on.
- **Commit**: `git log --oneline -1`.
- That nothing was pushed: CI confirms the fix once the maintainer pushes.

If you stopped early (already green, already fixed, infrastructure, flaky,
needs a decision), the report says which and why, with the evidence, and no
commit is made.
