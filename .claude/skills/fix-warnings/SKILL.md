---
name: fix-warnings
description: Run `cargo lint` (clippy with the pedantic group, the alias in `.cargo/config.toml`), collect every warning it reports and fix them — by changing the code where the lint has a point, or by a local `#[allow(..., reason = "...")]` where the code is deliberate. Verifies the result against the CI checks and leaves the changes uncommitted. Use when the user asks to fix warnings, fix lints, clean up clippy, or make `cargo lint` pass.
argument-hint: "[optional: files, modules or lint names to limit the fixes to]"
allowed-tools: Bash(cargo lint:*), Bash(cargo clippy:*), Bash(cargo build:*), Bash(cargo test:*), Bash(cargo fmt:*), Bash(git status:*), Bash(git diff:*), Bash(grep:*), Read, Edit, Grep, Glob
---

# Fix warnings

Make `cargo lint` report no warnings, without changing what jrs does.

`cargo lint` is an alias (`.cargo/config.toml`) for
`cargo clippy -- -W clippy::all -W clippy::pedantic` — stricter than CI, which
runs `cargo clippy --all-targets -- -D warnings`. It lints the library and
binary only, not the tests or benches.

## Hard rules

- **Behaviour stays the same.** A lint fix is a refactor: same output, same
  errors, same exit codes, same bytes on disk. If the only fix a lint offers
  would change behaviour, allow it instead and say so in the report.
- **Keep the project's invariants** (`CLAUDE.md`): no `println!`/`eprintln!`
  outside `ui/`, no `unwrap()` outside `#[cfg(test)]`, errors stay
  `JrsError` values, determinism (sorted traversal, fixed jar timestamps) is not
  touched. A lint never justifies breaking one of these.
- **Never silence lints wholesale.** No crate- or module-level `#![allow]`, no
  new entries in `Cargo.toml` `[lints]` or `.cargo/config.toml`, no removing
  `-W clippy::pedantic` from the alias. An exception is a local `#[allow]` on
  the one item it excuses, with a `reason`.
- **Don't touch existing `#[allow]`s** unless the lint they name no longer
  fires there (then remove the attribute) — they are decisions already made.
- **No new dependencies**, no `cargo clippy --fix` over the whole tree without
  reading the resulting diff, and no edits outside what the warnings point at.
- **Don't commit and never push.** Leave the changes in the working tree; the
  user can run `/commit`.

## 1. Collect

Run `git status` first; if there are uncommitted changes, mention it and work
on top of them — never discard them.

```
cargo lint 2>&1
```

Cargo replays cached diagnostics, so a second run shows the same warnings
without recompiling. `cargo lint` passes its trailing arguments to
clippy-driver, not cargo — add cargo flags (such as `--message-format=short`)
through the underlying command instead:

```
cargo clippy --message-format=short -- -W clippy::all -W clippy::pedantic
```

From the full output, list each warning: lint name (the `#[warn(clippy::…)]`
note, or the `help: for further information visit …#lint_name` link), file and
line, and the item it sits in. If `$ARGUMENTS` limits the scope to files,
modules or lint names, keep only those. If there are no warnings, say so and
stop.

Group the list by lint: the same lint usually wants the same treatment.

## 2. Decide, warning by warning

Read the code around each warning before touching it — the enclosing function,
and for a signature change, its callers (`Grep` for them). For each warning
pick one:

**Fix it** when the lint has a point and the fix is local and clear. Typical
cases:

- `doc_markdown` (missing backticks): put the identifier in backticks. If the
  flagged word is plain prose rather than code (a product name such as
  *JUnit* or *GitHub*), backticks are still the usual fix here; check how the
  surrounding docs write the same word and match them.
- `single_match_else` / `single_match`: rewrite as `if let … else`.
- `redundant_closure_for_method_calls`, `needless_pass_by_value`,
  `manual_let_else`, `uninlined_format_args` and other mechanical lints: apply
  the suggestion from clippy's `help:` line, adjusting callers where needed.
- `case_sensitive_file_extension_comparisons`: first decide which is right.
  A path on the user's disk usually wants `Path::extension` with
  `eq_ignore_ascii_case`; jar entry names and names javac or Spring look up
  exactly are case-sensitive on purpose — that is an `#[allow]` (see
  `src/package.rs` for both kinds).

**Allow it** when the code is deliberate and the lint's fix would make it worse
or wrong. Put the attribute on the narrowest item — the function, or the one
statement or expression — with a `reason` that says *why this code is right*,
not that the lint is noisy. Match the existing style:

```rust
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "jar entry names are case-sensitive; `Foo.CLASS` is not a class"
)]
```

`grep -rn -A3 'allow(' src` shows the exceptions already in the tree and the
tone of their reasons.

**`too_many_lines`** needs judgment, not a reflex:

- Split the function when it has clear seams — a block that computes one value,
  a loop body that is its own step, a match arm that is a sub-command — and the
  pieces get meaningful names and narrow signatures. Keep the new helpers next
  to the caller, private, in the order they run.
- Allow it when the length is the shape of the thing: one arm per command or
  table, a sequence of phases already delegated to helpers, one walk over
  shared state that splitting would thread through five parameters. The reason
  says which (see `src/manifest.rs`, `src/cli.rs`, `src/resolve/mod.rs`).
- Never shorten a function by squeezing formatting, deleting comments or
  inlining helpers.

**Casts** (`cast_possible_truncation`, `cast_sign_loss`,
`cast_precision_loss`): prefer `try_from`/`From` when a failure can be handled
or cannot happen by type; allow with a reason that states the bound
("the ratio is checked to be within 0..=1, so this is 0..=10000") when the
value is known to fit.

If a fix is not obvious, or it would ripple into a public signature used across
modules, or the lint seems to point at a real bug, stop on that one and list it
in the report rather than guessing.

## 3. Apply

Edit with `Edit`, one warning (or one group of identical warnings) at a time.
Keep the surrounding comment density, naming and idiom. After every few edits,
re-run `cargo lint` — a fix can surface a new warning (a split function tripping
`needless_pass_by_value`, say) or one clippy only reports after an earlier
error is gone.

Repeat until `cargo lint` reports no warnings in scope.

## 4. Verify

Run the CI checks; each must pass:

```
cargo fmt --check                              # if it fails: cargo fmt, then re-check
cargo clippy --all-targets -- -D warnings
cargo lint
cargo test
```

- `cargo test` needs a JDK for much of the build suite; tests without one skip
  with a `SKIPPED` line rather than failing. Report whether the JDK-backed
  tests ran, since a skipped run did not exercise a refactored build path.
- If a test fails, read the diff against the failure to find whether your
  change caused it, and fix the cause; do not change a test to make it pass.
- Read `git diff --stat` and the full `git diff` once: only the files the
  warnings pointed at, no unrelated reformatting.

## 5. Report

End with a short summary:

- **Fixed**: per lint, how many warnings and how (one line each).
- **Allowed**: each new `#[allow]` — the item, the lint and its reason.
- **Needs a decision**: warnings left in place, with why (behaviour change,
  suspected bug, cross-module signature change).
- The verification results, including the `SKIPPED` count from `cargo test`.

Don't commit; mention `/commit` if there are changes.
