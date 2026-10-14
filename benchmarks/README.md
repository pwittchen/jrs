# Benchmarks against Maven and Gradle

`cargo bench --bench compare` (`benches/compare.rs`) builds, tests, runs and
packages the same projects with jrs, Maven and Gradle, and writes the report
here. The plan, the scenarios and the fairness rules are
[specs/todo/BENCHMARKS.md](../specs/todo/BENCHMARKS.md); how to run it is in
[DOCS.md](../DOCS.md#development).

- `projects/<name>/` holds the Maven and Gradle builds of an example in
  `examples/<name>`, written by hand to the build its `jrs.toml` describes:
  the same dependencies at the same versions, the same `--release`, JUnit 5,
  and a fat jar as the package. The harness copies the example, lays these
  files over the copy, and checks that every tool resolves the same runtime
  classpath and runs the same tests before it times anything. The generated
  projects (`gen-300`, `gen-2000`, `gen-ws`) get all three build files from
  the generator in the harness, so they have no directory here.
- `REPORT.md` and `raw/<date>-<host>.tsv` are generated, never edited. A
  published report is committed by hand from the reference machine its
  header names.
