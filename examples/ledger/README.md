# ledger

A tiny double-entry bookkeeping tool, split into three modules and built
with jrs as one workspace. It exists to exercise multi-module builds: a
workspace root, members that depend on each other by path, one lockfile, and
packaging that carries a sibling's classes. It is not a place to keep your
accounts.

```
ledger/
├── jrs.toml        [workspace] members, and the [java] and [managed] they share
├── jrs.lock        one lockfile for all three
├── core/           Money, Posting, Transaction, Ledger: no dependencies
├── journal/        reads a CSV journal into a Ledger: core + commons-csv
└── app/            prints the balances: journal, and core through it
```

## Trying it

From this directory, with jrs installed (or substitute `cargo run --` from the
repository root plus `--manifest-path examples/ledger`):

```
jrs build                          # core, then journal, then app: three modules
jrs test                           # 6 tests in 3 modules
jrs run                            # the balances of app/src/main/resources/sample.csv
jrs run -- my-journal.csv          # or of your own journal
jrs -p journal test                # journal's tests, after building core
jrs -p app classpath               # app's classes, then journal's and core's, then the jars
jrs tree                           # each member's graph, its modules first
jrs package --fat                  # app: one jar with core, journal and commons-csv inside
java -jar app/target/app-1.0.0.jar
cd journal && jrs build            # inside a member: that member and what it depends on
jrs clean
```

The journal format is one posting per row; the rows of one transaction share
an `id`, and each transaction has to balance:

```
id,date,description,account,amount,currency
1,2026-01-01,Salary,assets:bank,2500.00,EUR
1,2026-01-01,Salary,income:salary,-2500.00,EUR
```

## What each part exercises

| In the project | What it tests in jrs |
| --- | --- |
| `[workspace] members = ["app", "journal", "core"]` | The members, listed out of order on purpose: the build order (core, journal, app) comes from the path dependencies, not from the list. |
| `[java]` and `[managed]` in the root `jrs.toml` | Shared keys: every member compiles for 17 with `-Xlint:all`, and every member's `junit-jupiter` takes its version from the one BOM. A member's own key would win. |
| `core = { path = "../core" }` in `journal` | A module dependency: core's classes go on journal's classpath where core's jar would, and core is compiled first. |
| `journal = { path = "../journal" }` in `app` | A module's own dependencies come with it, as a jar's do: app compiles against core's `Money` and gets commons-csv at runtime without naming either. |
| `"org.apache.commons:commons-csv"` in `journal` | An external dependency of a library member, with its own transitive ones (commons-io, commons-codec), resolved once for the whole workspace. |
| One `jrs.lock` at the root | Mediation runs once over every member's graph, so two members can never link against different versions of one library. The lockfile is `version = 3` and lists the members. |
| A body change in `core` | Compile avoidance across the module boundary: journal and app see core's API, not its bytes, so they stay fresh; a change to a public signature recompiles them. |
| `jrs package --fat` | app's fat jar carries core's and journal's classes under the same merge rules as any dependency; the libraries get the portable layout beside it. |

Two builds of the same sources produce byte-identical jars, wherever the
checkout is:

```
jrs clean && jrs package --fat && shasum app/target/app-1.0.0.jar
```

`jrs.lock` is committed, as it would be in a real project.
