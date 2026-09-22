# bookmarks

A small Spring Boot REST service — bookmarks kept in memory — built with jrs. It
exists to exercise the parts of the build system a Spring project leans on: a
BOM in `[managed]`, `javac -parameters`, resources, an application context in
the tests, and a fat jar that starts with `java -jar`. It is not a place to keep
your bookmarks.

```
GET    /api/bookmarks        the stored bookmarks
GET    /api/bookmarks/{id}   one, or 404
POST   /api/bookmarks        {"url": ..., "title": ...} -> 201 with a Location
DELETE /api/bookmarks/{id}   204, or 404
GET    /                     a page that lists them, from src/main/resources/static
```

## Trying it

From this directory, with jrs installed (or substitute `cargo run --` from the
repository root plus `--manifest-path examples/bookmarks`):

```
jrs build                          # resolve Boot's graph, compile, copy resources
jrs test                           # 8 tests: 4 plain, 4 through an application context
jrs test --filter '.*StoreTest'    # the ones that need no Spring at all
jrs run                            # http://localhost:8080, Ctrl-C to stop
jrs tree --depth 1                 # two roots; everything below is (managed)
jrs package --fat                  # one jar, ~18 MB, no Boot loader involved
java -jar target/bookmarks-1.0.0.jar
curl localhost:8080/api/bookmarks
jrs clean
```

With the application running:

```
curl -X POST localhost:8080/api/bookmarks \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","title":"Example"}'
```

## What each part exercises

| In the project | What it tests in jrs |
| --- | --- |
| `[managed]` with `spring-boot-dependencies`, `bom = true` | A BOM's managed versions settled before mediation: 72 jars resolve, and not one version is written in `jrs.toml`. |
| `"...:spring-boot-starter-webmvc" = {}` | A dependency with no version at all, taking it from the BOM. Upgrading the one version in `[managed]` upgrades the lot. |
| `javac-args = ["-parameters"]` | Pass-through compiler flags that the framework, not the compiler, needs: `BookmarkProperties` is bound by constructor, which works only if the parameter names survived. |
| `src/main/resources/application.properties` | Resource copying, read by Spring from the classpath — both from `target/classes` and from inside the jar. |
| `src/main/resources/static/index.html` | A resource served over HTTP, so a missing one is visible rather than silent. |
| `[test] java-agents = ["org.mockito:mockito-core"]` | A test-scoped artifact resolved from the graph and passed as `-javaagent:`, which is what Mockito asks for instead of self-attaching. |
| `BookmarkStoreTest` | Plain JUnit 5 from Boot's test starter: no context, milliseconds. |
| `BookmarkApiTest` (`@SpringBootTest`) | The JUnit launcher running a test that boots the whole application and drives it over MockMvc. |
| `jrs package --fat` | The fat jar's merge rules where they matter most: `META-INF/spring/*.imports` as a union of lines, `spring.factories` key by key. Auto-configuration lost here fails silently at startup. |
| `java -jar target/bookmarks-1.0.0.jar` | A flat fat jar starting a Boot application — no `bootJar`, no nested `BOOT-INF/lib`, and the package directory entries that Spring's component scan needs. |

Two builds of the same sources should produce byte-identical jars:

```
jrs clean && jrs package --fat && shasum target/bookmarks-1.0.0.jar
jrs clean && jrs package --fat && shasum target/bookmarks-1.0.0.jar
```

## What jrs does not do here

`jrs` is not the Spring Boot plugin. There is no `bootRun`, no dev-tools
restart, no `bootJar` layout, and no `spring-boot-configuration-processor`
metadata — `application.properties` gets no editor completion from this build.
What there is: the dependencies Boot's BOM pins, compiled and packaged into a
jar that starts.

`jrs.lock` is committed, as it would be in a real project. Delete it, or run
`jrs update`, to re-resolve from Maven Central.
