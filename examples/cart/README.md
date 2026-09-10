# cart

A small shopping-cart pricer in Java, specified with Spock, built with jrs. It
exists to exercise the most common way Groovy enters a Java project — its
tests — not to run a shop.

## Trying it

From this directory, with jrs installed (or substitute `cargo run --` from the
repository root plus `--manifest-path examples/cart`):

```
jrs build                          # Java only: javac, as ever
jrs test                           # the Spock specs: 20 tests, data tables unrolled
jrs test --filter '.*DiscountSpec'
jrs run                            # a sample cart with its best discount
jrs tree                           # Groovy and Spock marked (test)
jrs package --fat                  # no Groovy inside: it is a test dependency
java -jar target/cart-1.0.0.jar
jrs clean
```

## What each part exercises

| In the project | What it tests in jrs |
| --- | --- |
| `[groovy] version = "5.1.2"` | The Groovy compiler, resolved and pinned in `jrs.lock`, used only for the test unit. |
| `org.apache.groovy:groovy` in `[dev-dependencies]` | Declaring the runtime library replaces the implied one, so Groovy stays on the test classpath and out of the jar. It also wins nearest-wins against the 5.0.3 Spock asks for. |
| `src/main/java` and `src/test/groovy` | Two units in two languages: `javac` for the main code, groovyc for the specs. Main and test are separate compile units, so mixing them is fine. |
| `org.spockframework:spock-core` | Spock's own JUnit Platform engine. It brings `junit-platform-engine` 1.14, and the console launcher follows that version, since nothing JUnit is declared. |
| `*Spec` classes | With tests that are not all Java, jrs adds `.*Spec` and `.*Suite` to the launcher's class-name pattern; Spock specs would not run otherwise. |
| `Cart.Item`, a record | `java.source = 17` Java that the Groovy specs use. |

`jrs.lock` is committed, as it would be in a real project.
