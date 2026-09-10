# orders

A small order-pricing command-line tool in Kotlin and Java, built with jrs. It
exists to exercise jrs's Kotlin support on a realistic project, not to be a
good point-of-sale system.

```
orders [SKU:QUANTITY...]
```

With no arguments it prices a sample order.

## Trying it

From this directory, with jrs installed (or substitute `cargo run --` from the
repository root plus `--manifest-path examples/orders`):

```
jrs build                          # resolves and pins the Kotlin compiler too
jrs test                           # 7 tests: 4 in Kotlin, 3 in Java
jrs run                            # the sample order
jrs run -- cheese:2 coffee:10
jrs tree                           # kotlin-stdlib shows as (implied by [kotlin])
jrs tree --tool kotlin-compiler    # the compiler's own graph
jrs outdated                       # kotlin.version against the compiler's releases
jrs package --fat                  # self-contained jar, kotlin-stdlib inside
java -jar target/orders-1.0.0.jar apple:3
jrs clean
```

## What each part exercises

| In the project | What it tests in jrs |
| --- | --- |
| `[kotlin] version` | The compiler resolved as a graph of its own, pinned in `jrs.lock`'s `[[tool]]` block and run on the project's JDK; `kotlin-stdlib` implied at the same version. |
| `kotlinx-coroutines-core` | A Kotlin Multiplatform library: its root POM is `pom`-packaged and points at `kotlinx-coroutines-core-jvm`. It asks for `kotlin-stdlib` 2.1.0, and the implied 2.4.20, a direct dependency, wins nearest-wins; the first build's warning says so. |
| `kotlinc-args = ["-Werror"]` | Compiler flags passed through after jrs's own. The sources compile without warnings, so any warning is news. |
| `Order.kt` and `LegacyPricing.java` | Mixed sources both ways: kotlinc reads the Java for its symbols, then `javac` compiles it against the Kotlin classes. |
| `Main.kt`'s top-level `main` | `main-class = "com.example.orders.MainKt"`: the class a file-level `main` compiles to. |
| `internal fun Order.subtotalCents`, called from `OrderTest` | The test unit compiled as a friend of the main module (`-Xfriend-paths`). |
| `LegacyPricingTest.java` in `src/test/java` | A Java test in a Kotlin test unit: kotlinc, then `javac`, into `target/test-classes`. |
| `kotlin-test-junit5` | `kotlin.test` on JUnit 5. It brings an older `junit-platform-launcher`, which jrs keeps off the test JVM's classpath so the console launcher runs with its own. |
| `java.source = 17` | One release for both compilers: `-jvm-target 17 -Xjdk-release=17` for kotlinc, `--release 17` for `javac`. |

Two builds of the same sources should produce byte-identical jars:

```
jrs clean && jrs package --fat && shasum target/orders-1.0.0.jar
jrs clean && jrs package --fat && shasum target/orders-1.0.0.jar
```

`jrs.lock` is committed, as it would be in a real project, and pins the Kotlin
compiler's graph beside the project's.
