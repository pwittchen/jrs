# calc

A small arithmetic calculator in Scala 3 and Java, built with jrs. It exists to
exercise jrs's Scala support on a realistic project, not to replace `bc`.

```
calc [EXPRESSION...]
```

With no arguments it evaluates a few sample expressions.

## Trying it

From this directory, with jrs installed (or substitute `cargo run --` from the
repository root plus `--manifest-path examples/calc`):

```
jrs build                          # resolves and pins the Scala 3 compiler too
jrs test                           # 6 MUnit tests, on the Vintage engine
jrs run                            # the samples
jrs run -- "2 * (3 + 4)" "1 +"
jrs tree                           # the Scala library, twice (implied by [scala])
jrs tree --tool scala-compiler     # the compiler's own graph
jrs package --fat                  # self-contained jar, the Scala library inside
java -jar target/calc-1.0.0.jar "6 / 4"
jrs clean
```

## What each part exercises

| In the project | What it tests in jrs |
| --- | --- |
| `[scala] version = "3.9.0"` | `scala3-compiler_3` resolved as its own graph and pinned in `jrs.lock`. From 3.8 on, the standard library is `scala-library` 3.x and `scala3-library_3` a shim over it, so both are implied. |
| `org.scalameta:munit_3` | A test framework built against Scala 3.3: it brings `scala3-library_3` 3.3.8 and `scala-library` 2.13, and the implied 3.9.0 pair wins both, as the first build's warnings say. It is a JUnit 4 runner and brings `junit:junit`, so jrs picks the Vintage launcher without JUnit being declared. |
| `CalculatorSuite` | Named `*Suite`: with tests that are not all Java, jrs adds `.*Spec` and `.*Suite` to the launcher's class-name pattern. |
| `Main.scala` and `Format.java` | Mixed sources both ways: scalac reads the Java for its symbols, then `javac` compiles it against the Scala classes, calling `Calculator` through its static forwarder. |
| `scalac-args` | `-deprecation -feature -Werror` passed through after jrs's own flags; the sources compile without warnings. |
| `java.source = 17` | One release for both compilers: `-java-output-version 17` for scalac, `--release 17` for `javac`. |

`jrs.lock` is committed, as it would be in a real project, and pins the Scala
compiler's graph beside the project's.
