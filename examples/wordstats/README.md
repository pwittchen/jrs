# wordstats

A small command-line word counter, built with jrs. It exists to exercise every
part of the build system on a realistic project, not to be a good word counter.

```
wordstats [--top N] [--format text|json] [FILE...]
```

With no files it counts a sample text bundled as a resource.

## Trying it

From this directory, with jrs installed (or substitute `cargo run --` from the
repository root plus `--manifest-path examples/wordstats`):

```
jrs build                         # resolve, compile, copy resources
jrs test                          # 11 JUnit 5 tests
jrs test --filter '.*Report.*'    # a subset
jrs run                           # top 10 words of the bundled sample
jrs run -- --format json --top 3 src/test/resources/fixture.txt
jrs tree                          # the resolved graph, test scope marked
jrs package                       # thin jar, Class-Path into the cache
jrs package --fat                 # self-contained jar
java -jar target/wordstats-1.0.0.jar --top 5
jrs clean
```

## What each part exercises

| In the project | What it tests in jrs |
| --- | --- |
| `com.google.guava:guava` | Transitive resolution through a parent POM (6 transitive jars, including the `listenablefuture` empty-jar trick). |
| `org.apache.commons:commons-lang3` | The long-form `{ version = ... }` dependency syntax. |
| `org.junit.jupiter:junit-jupiter` | `[dev-dependencies]`: test-scoped in `jrs tree`, on the test classpath, absent from the fat jar, and the JUnit launcher being pulled in. |
| `java.source = 17`, `-Xlint:all` | `--release` and pass-through `javac-args`. The sources compile without warnings, so any warning is news. |
| Two packages, seven classes | Source globbing across directories. |
| `src/main/resources/wordstats/*.txt` | Resource copying, read at runtime through `Class.getResource`. |
| `src/main/resources/META-INF/services/...` | `ServiceLoader` discovery: `--format` finds its implementations only if the services file reached the classpath and the jar. |
| `src/test/resources/fixture.txt` | Test resources: on the test classpath, never in the jar. |
| `Main` exits `2` on a bad flag | `jrs run` passing the program's exit code through (`jrs -q run -- --top 0`). |

Two builds of the same sources should produce byte-identical jars:

```
jrs clean && jrs package && shasum target/wordstats-1.0.0.jar
jrs clean && jrs package && shasum target/wordstats-1.0.0.jar
```

`jrs.lock` is committed, as it would be in a real project. Delete it, or run
`jrs update`, to re-resolve from Maven Central.
