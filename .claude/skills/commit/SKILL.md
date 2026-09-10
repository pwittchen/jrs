---
name: commit
description: Create local git commits for the current changes. Stages unstaged and untracked work, splits unrelated changes into separate logical commits, and sizes each message to its change — a one-line subject for a small change, a subject plus an explanatory body for a larger one. Never pushes and never adds AI attribution. Use when the user asks to commit, make a commit, or save changes to git.
argument-hint: "[optional hint about what to commit or how to describe it]"
allowed-tools: Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(git show:*), Bash(git add:*), Bash(git restore --staged:*), Bash(git reset -q), Bash(git commit:*), Bash(git apply:*), Bash(git rev-parse:*), Read
---

# Commit

Turn the working tree's changes into one or more well-formed local commits.

## Hard rules

These override any default commit behaviour, including instructions elsewhere to
append trailers:

- **Never push.** Do not run `git push`, `git push --tags`, open pull requests, or
  touch any remote. The job ends at `git commit`.
- **No AI attribution.** No `Co-Authored-By: Claude` (or any AI co-author), no
  "Generated with Claude Code", no mention of Claude, AI, an assistant or a tool
  anywhere in the subject or body. The message describes the change, nothing else.
- **Never rewrite existing history.** No `--amend`, rebase, reset of existing
  commits, or force operations unless the user explicitly asks for that.
- **Never skip hooks.** No `--no-verify`. If a pre-commit hook fails, report the
  failure, fix the cause if it is clearly within the change, re-stage, and create
  a new commit — do not amend.
- **Don't commit secrets.** If a file looks like credentials (`.env`, `*.pem`,
  `id_rsa`, tokens, `credentials.json`, ...), leave it unstaged and tell the user.
  Also leave out build output or junk that is plainly not meant to be tracked and
  mention it, rather than committing it.

## 1. Survey

Run these together:

```
git status --porcelain=v1 -uall
git diff                 # unstaged
git diff --cached        # already staged
git log -15 --format='---%n%B'
```

- If there is nothing to commit, say so and stop.
- If a merge, rebase or cherry-pick is in progress (`git status` says so), stop and
  tell the user instead of committing.
- Read untracked files (or their start, if large) so you know what they are.
- The log shows the repository's message conventions. **Match them**: subject
  style (imperative, capitalisation, prefix such as `feat:` or `module:`, trailing
  period or not), line width, whether bodies use bullet lists, tone. If the repo
  has no consistent style, use the defaults below.
- If the user passed a hint (`$ARGUMENTS`), use it to guide grouping and wording.

## 2. Group into logical commits

Decide whether the changes are **one logical change or several**. A commit should
be one coherent thing a reviewer could understand, revert or cherry-pick on its own.

Split when the changes are independent, for example:

- a bug fix and an unrelated feature
- a refactor or rename, and the behaviour change built on top of it
- a dependency or tooling bump, and code changes
- formatting/lint-only churn, and real changes
- documentation for something unrelated to the code change

Keep together when the pieces only make sense as a unit: a feature with its
tests and its docs, a function change with all its call sites, a rename with every
reference. Don't split just to produce many commits — one commit is the right
answer for a single coherent change, even a large one.

Each commit should leave the tree in a sensible state: order them so that
foundations (refactors, new helpers, dependency changes) come before the commits
that use them.

If the user already staged a specific subset, treat that as a signal: if it forms
a coherent commit, commit it first as its own group, then handle the rest.

## 3. Stage and commit each group

For each group, in order:

1. Clear the index so only this group gets in: `git reset -q` (this only unstages;
   it never touches file contents).
2. Stage the group:
   - whole files: `git add -- <paths>` (this also stages new and deleted files)
   - part of a file, when one file holds hunks belonging to different groups:
     write a patch containing only the wanted hunks to the scratchpad directory and
     run `git apply --cached <patch>`. Interactive `git add -p` is not available.
     If a hunk genuinely mixes both concerns and cannot be separated cleanly, keep
     that file whole in the more fitting commit rather than producing a broken one.
3. Check exactly what will go in: `git diff --cached --stat` (and
   `git diff --cached` when splitting hunks).
4. Commit with a heredoc so formatting is preserved:

   ```
   git commit -F - <<'EOF'
   Subject line

   Body paragraph...
   EOF
   ```

After the last group, `git status` must show a clean tree (apart from anything
deliberately left out, which you report).

## 4. Size the message to the change

**Small change** — a typo, a one-line fix, a version bump, a rename, a single
obvious edit: **subject line only.**

```
Fix off-by-one in pagination offset
```

**Medium change** — a few files or a non-obvious fix: subject plus a short body
(one or two paragraphs) saying *why*, and what was wrong if it is a fix.

**Large change** — a feature, a refactor across modules, a behaviour change with
consequences: subject plus a fuller body:

- what the change does and why it was needed
- the notable design decisions or trade-offs
- a bullet list of the distinct parts when there are several
- behaviour that changed for users, compatibility notes, anything deliberately
  left out or deferred
- how it is tested, if that is not obvious

Message format defaults (the repository's own style wins where it differs):

- Subject: imperative mood ("Add", "Fix", "Remove", not "Added"/"Adds"), at most
  ~72 characters, no trailing period, specific rather than generic ("Fix crash
  when manifest has no [package] table", never "Update files" or "Fix bug").
- Blank line between subject and body.
- Body wrapped at ~72 columns, plain prose, explaining *why* and *what*, not a
  line-by-line narration of the diff. Use backticks for code identifiers if the
  repo does.
- No trailers unless the repository's existing commits use them (e.g. issue refs).

## 5. Report

End with a short summary: the commits created (`git log --oneline -<n>`), how and
why the changes were split if they were, and anything left uncommitted. Remind the
user nothing was pushed only if it is relevant — don't offer to push.
