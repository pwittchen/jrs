---
name: update-docs
description: Review the jrs project against its Markdown documentation and update whatever has gone out of date — README, DOCS.md, ARCH.md, CLAUDE.md, ROADMAP.md, the specs and the example READMEs. Checks every factual claim (commands, flags, manifest keys, module and file names, test suites, crates, CI, status lines, links) against the code, fixes drift in the docs only, and reports anything that looks like a code bug or needs a decision instead of editing it. Use when the user asks to update, refresh, sync, audit or check the docs.
argument-hint: "[optional: files to limit the review to, or a git ref to review changes since]"
allowed-tools: Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(git show:*), Bash(git ls-files:*), Bash(git rev-parse:*), Bash(cargo run:*), Bash(cargo build:*), Bash(python3 .claude/skills/update-docs/check_links.py:*), Bash(ls:*), Bash(find:*), Bash(grep:*), Bash(wc:*), Read, Edit, Grep, Glob, Agent
---

# Update docs

Bring the repository's Markdown documentation back in line with the code. The
code is the source of truth for what jrs *does*; the docs are edited to match
it, never the other way round.

## Hard rules

- **Edit `*.md` files only.** Never change code, tests, `Cargo.toml`, fixtures
  or the workflow to make them agree with a doc. If the code looks wrong and the
  doc right, that is a finding to report, not something to fix here.
- **Every edit needs evidence.** Change a claim only after reading the code,
  help text or file that contradicts it. If you cannot confirm a claim either
  way, leave it and list it under "unverified" in the report. Never invent
  flags, keys, numbers or behaviour.
- **Respect what each document is** (see the table below). The specs are design
  records with their own rules; `ROADMAP.md` lists only what is left; example
  READMEs describe their own example.
- **Don't "fix" the deliberate divergences.** `specs/INITIAL_SPEC.md` §12.1
  lists where the code intentionally differs from the spec. A mismatch covered
  there is correct as it stands.
- **Keep the voice.** British spelling (*behaviour*, *licence*, *organised*),
  plain declarative prose, the existing wrap width of each file (~80 columns),
  the existing heading and table style. Edit sentences in place; don't rewrite
  sections that are still correct, reorder them, or add marketing.
- **Minimal diffs.** Prefer changing a word, a number, a path or a list entry
  over rewriting a paragraph. Add a new section only for a user-visible feature
  or a module that has none, and model it on its neighbours.
- **Don't commit and never push.** Leave the changes in the working tree; the
  user can run `/commit`. Don't bump versions.

## The documents and their ground truth

| Document | What it is | Check it against |
| --- | --- | --- |
| `README.md` | A short overview: status, feature summary, quick install, a first project, links into `DOCS.md`. Keep it short — details belong in `DOCS.md`. | The feature summary against `DOCS.md`; the install commands against `website/install.sh`; every link and `DOCS.md#…` anchor resolves. |
| `DOCS.md` | The user manual: install, manifest reference, commands, config, tests, tasks, languages, migration, development. | `cargo run -q -- --help` and `cargo run -q -- <cmd> --help` for every command; `src/manifest.rs` (accepted keys, defaults, validation), `src/config.rs` (user config), `src/cli.rs` (behaviour), `src/compile/lang.rs` (languages, default versions), `src/task.rs` (task keys, placeholders, env vars), `src/resolve/cache.rs` (cache location), `src/completions.rs` (shells), `.github/workflows/rust.yml` (release assets, targets). |
| `ARCH.md` | The architecture map: source map, module layers, `Session` spine, resolution, compile units, output layer, files on disk, invariants. | The `src/` tree (`find src -name '*.rs' \| sort`), `mod` declarations in `src/lib.rs` and each `mod.rs`, `use` statements (which module depends on which), the functions and types it names, the files actually written under `target/` and `target/.jrs/`, `tests/`. |
| `CLAUDE.md` | Guidance for Claude Code: commands, CI, architecture summary, invariants, test layout, crate list. | `Cargo.toml` (dependencies, features, benches), `.github/workflows/rust.yml` (jobs, OSes, `paths-ignore`, release steps), `tests/*.rs` and `tests/fixtures/`, `benches/`, the `require_jdk!` macro. Keep it consistent with `ARCH.md`, which it summarises. |
| `ROADMAP.md` | What is **left** to do, and nothing else — it keeps no list of what has landed. | Grep the code for each remaining item. An item that has been implemented is removed; don't mark it done in place or record it elsewhere in the roadmap. A landed piece of a larger item goes too, leaving only the context the open part needs. Adding new items is the user's call — suggest, don't add. |
| `specs/INITIAL_SPEC.md` | The design contract the code follows; module doc comments cite it by section. | Only update: milestone status in §12, cross-references, file and module names, and statements the code has plainly superseded in a way already reflected elsewhere (ARCH.md, DOCS.md). A behavioural mismatch that §12.1 doesn't cover is a **finding** — report it with both sides; don't silently rewrite the spec or add a §12.1 entry. Never renumber sections: code cites them. |
| `specs/JVM_LANGUAGES.md`, `specs/TASKS.md` | Design records, "corrected where the implementation settled a detail differently". | Their `Status:` line (what is and isn't built), and details the implementation settled differently — those may be corrected in place, as the documents themselves say. Leave the problem statements, arguments and spike notes alone. |
| `examples/*/README.md` | What each example exercises and how to try it. | That example's `jrs.toml`, `jrs.lock`, sources and tests: versions, dependency coordinates, file and class names, test counts (count the test methods), commands shown. |

`.claude/skills/*/SKILL.md` are out of scope unless the user names them.

## 1. Scope

- If `$ARGUMENTS` names files, review only those.
- If `$ARGUMENTS` is a git ref, or the user asks for "what changed", review
  what changed since it: `git log --oneline <ref>..HEAD` and
  `git diff --stat <ref>..HEAD -- ':!*.md'`, then pick the documents that talk
  about the touched areas.
- Otherwise review everything. To prioritise, find the code that changed after
  each document was last touched:

  ```
  git log -1 --format='%h %ad %s' --date=short -- <doc>
  git log --oneline <that-hash>..HEAD -- src tests Cargo.toml .github
  ```

  Code commits the docs never caught up with are the most likely source of
  drift, but still review each document in full — drift also comes from the
  docs being wrong from the start.

Run `git status` first. If there are uncommitted changes to `*.md` files,
mention it and build on them rather than discarding them.

## 2. Collect ground truth

Gather the facts the documents are checked against, once, before reading them:

- `cargo run -q -- --help`, then `--help` for each listed subcommand (and
  nested ones such as `cache`). This is the authority on commands and flags.
- `find src tests benches -name '*.rs' | sort` and `ls tests/fixtures`.
- `Cargo.toml` and `.github/workflows/rust.yml`.
- The manifest keys `src/manifest.rs` accepts (search for the key strings and
  the unknown-key warning), and the user config keys in `src/config.rs`.

## 3. Review the documents

The documents are long (DOCS ~1100 lines, ARCH ~680, INITIAL_SPEC ~1400).
For a full review, fan out: launch one `general-purpose` agent per document or
group (README + DOCS.md; ARCH.md + CLAUDE.md; ROADMAP.md + the three specs; the example
READMEs), **in a single message so they run in parallel**. Give each agent:

- the document(s), its row from the table above, and the ground truth from
  step 2 (or tell it how to collect it),
- the instruction to **read, not edit**: go claim by claim, verify each against
  the code, and return a list of discrepancies. Each entry: file and line, the
  current text quoted exactly, what the code actually says with a
  `path:line` reference, and the proposed replacement text,
- the instruction to separately list claims it could not verify, and apparent
  code bugs (doc right, code wrong),
- the voice rules from above, so the proposed text is ready to apply.

For a narrow review (one or two short files), do it directly instead.

What to check, claim by claim:

- **Commands and flags**: every command, flag, short option, default value and
  example invocation exists and behaves as described; no command or flag
  in `--help` is missing from the command list in `DOCS.md`.
- **Manifest and config keys**: names, types, defaults, which table they sit
  in; new keys documented, removed keys gone.
- **Names**: modules, files, functions, types, `target/` paths, test suites,
  fixtures, env vars (`JRS_CACHE_DIR`, `JAVA_HOME`, …) all exist as spelled.
- **Numbers and versions**: default language versions, JDK minimum, test counts
  in examples, line counts or counts of anything ("four places", "three
  suites") still hold.
- **Lists that should be complete**: the crate list, CI jobs and OSes,
  supported shells, the §12.1 divergence count mentioned in `CLAUDE.md`, the
  README feature summary.
- **Status lines**: specs' `Status:`, ROADMAP items, "not built" or "not yet"
  statements anywhere — grep for `not yet`, `not built`, `TODO`, `planned`,
  `future`, `will ` and check each.
- **Cross-document consistency**: `CLAUDE.md` agrees with `ARCH.md`; DOCS.md
  agrees with SPEC §5 (CLI) and §4 (manifest) except where §12.1 says otherwise.

## 4. Apply

Collect the agents' findings and verify each one yourself before editing —
open the cited code and confirm it; agents are wrong sometimes. Drop anything
that doesn't hold up. Then apply the edits with `Edit`, one precise change at a
time, following the rules above.

Hold back, and put in the report instead of editing:

- apparent code bugs, and spec/code mismatches not covered by §12.1,
- new ROADMAP items, new §12.1 entries, or any change to a design decision,
- anything that would mean restructuring a document.

## 5. Check

- `python3 .claude/skills/update-docs/check_links.py` — relative links and
  `#anchor`s across all tracked Markdown. Fix any link your edits broke; report
  pre-existing broken links you couldn't resolve. A renamed heading breaks
  anchors elsewhere, so grep for links to it.
- `git diff --stat -- '*.md'` and read the full `git diff` once: no unintended
  reflows, no non-`*.md` files touched, voice and wrap width kept.

## 6. Report

End with a short summary:

- **Updated**: per file, what was out of date and what it says now (one line
  each, not the diff).
- **Needs a decision**: code/spec mismatches, suspected code bugs, suggested
  ROADMAP items — each with the evidence.
- **Unverified**: claims you couldn't confirm either way.
- If nothing was out of date, say so plainly.

Don't commit; mention `/commit` if there are changes.
