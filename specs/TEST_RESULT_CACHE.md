# jrs — Per-class test results in the build cache

Design proposal for the last test item in
[ROADMAP §6](../ROADMAP.md#6-faster-than-gradle), and open question 7 of
[FASTER_BUILDS.md](FASTER_BUILDS.md#8-open-questions): letting a branch
switch, a second checkout or a CI machine reuse *part* of a test suite's
results, class by class, instead of all of them or none.

Status: **proposal, nothing implemented.** It builds on the build cache of
SPEC §7.8 and on test selection (`compile/impact.rs`, SPEC §10.2), both of
which have landed. §6 lists the SPEC.md edits it needs before it has code.

---

## 1. Problem

Two mechanisms skip test work today, and neither does what a branch switch
needs.

**Test selection** (`impact.rs`) is per checkout. After every run it
records in `target/.jrs/test.tested` every file in the class directories
with its hash, and the next `jrs test` runs only the test classes whose
constant pools reach a changed class. It is cheap and local, and it knows
nothing about runs made anywhere else: a fresh checkout, a `jrs clean` or a
CI machine has no record, so `select` answers `Selection::All("there is no
earlier run to compare with")`. A branch switch that changes one class out
of 2,000 is compared with the *last run in this directory*, which may have
been on the other branch, and often reaches far more than the one class.

**The cached run** (F5, `Session::cached_tests` / `restore_tests` /
`store_tests` in `cli.rs`) is cross-checkout but all or nothing. Its key is
the run's settings — `jvm-args`, launcher version, scan directory, `test.env`,
every jar by identity — plus the hash of *every* file in the class
directories. One changed class anywhere misses the whole entry, and the
whole suite runs.

What is missing is the combination: a key per test class, covering only what
that class can reach, held in the shared build cache. A branch switch that
touches one class then runs the tests that reach it and takes the rest from
the cache, whichever checkout or machine ran them.

## 2. Principles

FASTER_BUILDS §2 holds unchanged. The ones that bite here:

- **Errs towards running.** A class whose reach cannot be vouched for is not
  served from the cache. A spurious run costs seconds; a cached pass for a
  test that would now fail is a wrong result, and on CI a wrong green.
- **Never a failure of its own.** A cache that cannot be read or written, an
  entry that does not parse: the class runs.
- **Only passes are kept.** A class with a failure, an error or a flaky test
  is never stored, so a cached result is always a pass (or a skip).
- **What CI reads stays whole.** `target/test-reports/` holds JUnit XML for
  every test class the run accounts for, cached or fresh, and `index.html`
  lists them all.
- **No new crate.** `quick-xml` already reads the reports; writing them back
  needs a few dozen lines, not a dependency.

## 3. The key

### 3.1 What a class's outcome depends on

The run-level part is what `cached_tests` already hashes, unchanged: JVM
arguments, launcher version, scan directory and `test.env`, all relative to
`{root}`/`{cache}`, and every jar by coordinate and pinned checksum. Any
change there misses every class, as it misses the whole-suite entry now.

The class-level part is its **reach**: the top-level test class, its nested
classes, and every project class reachable from them through constant-pool
references, transitively — the same walk `impact::finding_classes` does over
`abi::ClassInfo::refs`, restricted to classes in the class directories. The
key is SHA-256 over:

```
jrs build cache 1 / test-class
<run settings, as cached_tests builds them, without the class-file list>
class com.example.CalcTest
reach <relative path> <sha256>     # every class file reached, sorted
resources <sha256>                 # every resource file, as one digest
```

### 3.2 What the walk cannot vouch for

`impact.rs` already names what the class files do not show, and each case
gets the same answer it gets there:

| Case | Today in `impact.rs` | Per-class key |
| --- | --- | --- |
| A resource (`application.yml`, a fixture file) | runs every class | every resource is in every key: one resource change misses all classes |
| A reach that includes a `DYNAMIC` prefix (Spring's test context, `ServiceLoader`, `java.lang.reflect`, ArchUnit, …) | runs on every change | the key covers *every* class file, not the reach: such a class is cached only as long as nothing in the class directories changed |
| A Groovy class anywhere | runs every class | no per-class keys at all; only the whole-suite entry of F5 |
| A class that cannot be read | runs every class | as Groovy |
| A `@Suite` class (`org/junit/platform/suite/`) | in `DYNAMIC` | as Groovy: a suite's report names the classes it ran, not itself, so its results cannot be filed under one key |

Recommended: implement exactly these, with `DYNAMIC` and the Groovy check
shared with `impact.rs` rather than copied. A Spring Boot suite whose tests
all start a context therefore gains nothing from per-class keys over the
whole-suite entry; a library's unit tests gain the most. That is the honest
shape of what constant pools can prove.

### 3.3 Cost

Reach needs every class file's `ClassInfo`. `impact::compare` reads them
only when there is a previous record and something changed; a fresh
checkout has neither, which is exactly when the cache matters. Reading and
parsing 5,000 class files is in the order of 100–200 ms, small next to a
test JVM, but paid on every run that consults the cache.

Recommended: keep each class file's `refs` in `target/.jrs/test.reach`,
keyed by the file's hash, which the impact snapshot already computes. A run
then parses only class files whose hash is new. The record is scratch space
like `test.tested` and is rebuilt when missing.

## 4. Entries and reports

### 4.1 What an entry holds

The launcher writes one XML file per engine (`TEST-junit-jupiter.xml`,
`TEST-junit-vintage.xml`), not one per class, so an entry cannot be a file the
launcher wrote. Two options:

1. split the launcher's XML into per-class fragments, byte for byte;
2. store the class's parsed cases and write XML from them on restore.

Recommended: **2**. `test_report::parse_report` already turns a report into
`TestCase` values — class, name, time, status, skip reason, unique ID,
display name, and the `<system-out>` jrs reads the unique ID from. An entry
is a small canonical file of those values plus the engine it ran on (the
first segment of the unique ID). Splitting XML byte for byte would need a
round-trip-exact XML editor for the launcher's output; jrs owns neither its
format nor its versions.

### 4.2 Restoring into `test-reports/`

A run with some classes cached and some run fresh leaves:

```
target/test-reports/
├── TEST-junit-jupiter.xml           the launcher's, for the classes it ran
├── TEST-junit-jupiter-cached.xml    written by jrs, one <testsuite> per engine
├── retry-<n>/                       as today, fresh classes only
└── index.html                       written from all of them
```

`test_report::load` already reads every `TEST-*.xml` directly in the
directory, so the HTML page, `--rerun-failed` and flaky counting need no
change. CI tools that read `TEST-*.xml` (Surefire's layout, which GitHub,
GitLab and Jenkins all accept) see every class once. A cached case's
`<system-out>` carries `cached: <key>` after jrs's usual `unique-id` line,
so a reader can tell where a result came from.

Forks keep writing `fork-<n>/` and hoisting their XML up as today; the
cached file is written once, by the parent, before the launchers start.

### 4.3 Counts and the transcript

The outcome's `found`, `passed` and `skipped` add the cached cases to the
launcher's summary block. The `Testing` line says what ran and what did not:

```
     Testing 12 of 214 test classes (202 passed in cached runs)
     Testing 214 test classes: passed in cached runs (`--all` runs them)
    Finished 1,530 tests, 1,530 passed (1,402 from the cache) in 2.1s
```

The second line is F5's, kept for the case where nothing runs. Under `-v`,
each cached class is named with its key.

## 5. Flow

```
 impact::select(state, …)
   ├── Only([])  ──────────────────────► "no test class reaches a change", done
   ├── Only(selected) ─┐
   └── All(reason) ────┤  candidates = selected, or every test class
                       ▼
          whole-suite entry (F5)? ── hit ─► restore it all, done
                       │ miss
                       ▼
          per class in candidates: key ─► entry?  hit ─► cached
                       │                          miss ─► to run
                       │  (test.tested's pending always runs:
                       │   a failure is never stored, so it misses anyway)
                       ▼
          write TEST-*-cached.xml; run the rest (forks split only them)
                       ▼
          each fresh class that passed, with no flaky case ─► store its entry
          a whole run that passed ─► also the whole-suite entry, as today
```

Recommended: test selection runs first and the cache is consulted only for
the classes it selects. Selection is local and costs a hash comparison; the
cache costs a lookup per class, and may cost a network request. A class
selection leaves out is one this checkout already ran on these inputs.

The whole-suite entry stays, as the one-lookup path for the commonest CI
case (a commit already built on another branch) and because it covers
Groovy and suite-engine projects, which get no per-class keys.

`test.times` keeps each cached class's last measured time, so that fork
balancing is not skewed by zeros. Retries rerun fresh failures only; a cached
class never failed. `--coverage`, `--debug`, `--all`, `--rerun-failed`,
`--filter`, tags and `--method` neither read nor write per-class entries,
exactly as for the whole-suite entry.

## 6. Correctness: what a class's key does not cover

**Static and shared state.** A test class can pass in a run because another
class ran before it in the same JVM — a static cache warmed, a system
property set, a database row written — and fail alone, or the reverse. Its
key covers its own reach, not the run's order.

This risk is not new: test selection already runs subsets, and `test.forks`
already splits classes among JVMs by measured time, so jrs has run classes
apart from the classes they ran with since both landed. Per-class caching
widens the window from "within a checkout" to "across checkouts", which is
why it is bounded as follows:

- a class is stored only from a run where it passed *and* every class in that
  run passed, so one failing neighbour leaves its whole run uncached;
- `jrs test --all` always runs everything and refreshes every entry;
- `[test] cache = false` (new, default `true` while the build cache is on)
  turns test caching off for a project whose classes are known to share
  state, without turning off compile caching.

**External state.** As for the whole-suite entry (SPEC §7.8): a test reading
an environment variable outside `test.env`, a file outside the class
directories, or a service, is outside the key. The per-class cache does not
change that trust; it does make it apply to more runs, which is one more
reason for the `[test] cache` switch.

**Parameterized, dynamic and nested tests** need nothing special. Every case
of a `@ParameterizedTest` or `@TestFactory` is a `<testcase>` whose
`classname` is its class; nested classes are filed under their top-level
class, as `impact::top_level` and `test::fork_pattern` already do.

## 7. SPEC.md changes

1. SPEC §7.8: the test-results paragraph gains per-class entries, their key
   and its fallbacks (§3), and the restored XML file (§4.2).
2. SPEC §10.2: the selection pipeline of §5, and the `Testing` and `Finished`
   lines of §4.3.
3. SPEC §4.2: `test.cache`.
4. ARCH §8: the flow diagram; ARCH §13: `target/.jrs/test.reach`.

## 8. Open questions

1. **Remote lookups.** A 2,000-class suite is 2,000 `GET`s on a cold local
   cache. Fetching them `--jobs` at a time may still be slower than running a
   fast unit suite. A per-run index entry — the whole-suite key mapped to the
   per-class keys it stored — would let one `GET` say which entries exist,
   but only for an exact run that was pushed. To be measured on the corpus
   before remote per-class lookups are turned on; until then they are local
   only.
2. **Stored output.** A passing test's `<system-out>` can be large (log lines).
   Keeping it makes a restored report identical to a fresh one; dropping it
   keeps entries small. Proposal: keep it, capped at 64 KB per class.
3. **The `[test] cache` default.** On, like the build cache, or off until
   the corpus has run with it? The bound of §6 suggests on; the cost of a
   wrong green on CI suggests off. Decide with the corpus.
4. **JUnit 4 under Vintage** reports classes the same way, but rules and
   runners (`@RunWith(Suite.class)`, Spring's runner) find classes by name.
   Whether `DYNAMIC` needs Vintage-specific prefixes (`org/junit/runners/Suite`)
   is to be checked on the corpus's JUnit 4 projects.

## 9. Implementation plan

### 9.1 Milestones

| | Part | Measured by |
| --- | --- | --- |
| T1 | Reach per test class, shared with `impact.rs`; `test.reach` | the time a run spends keying, fresh checkout, 5,000 classes |
| T2 | Per-class entries, local only; the cached XML file; counts and transcript | `test` after a branch switch that changes one class |
| T3 | `[test] cache` | — |
| T4 | Remote per-class lookups, behind §8 question 1 | the same, from a warm remote on an empty local cache |

### 9.2 Code layout

- `compile/impact.rs`: `reach(classes, test) -> BTreeSet<&str>` and the
  `DYNAMIC`/Groovy/suite checks, public to the cache; `test.reach`.
- `test_report.rs`: a writer for `TestCase` values back to JUnit XML, and the
  entry format.
- `cli.rs` (`Session::test`): the flow of §5 between selection and launch;
  `cached_tests` grows a per-class variant.
- `build_cache.rs`: unchanged; entries are `Entries` like any other.

### 9.3 Tests

All hermetic, against the fake launcher and the `file://` fixture, as the
rest of `tests/build.rs` is:

- two checkouts sharing `JRS_CACHE_DIR`; the second changes one class reached
  by one test class out of three: one runs, two are restored, the reports
  hold all three, `index.html` lists all three, the `Finished` counts add up;
- a changed resource runs every class; a class reaching a `DYNAMIC` prefix
  misses on any class-file change; a Groovy test class turns per-class keys
  off;
- a failing class leaves its whole run unstored, and a flaky one too;
- forks split only the classes that run, and `test.times` keeps the cached
  classes' times;
- `--all` runs everything; `[test] cache = false` stores nothing;
- unit tests in `test_report.rs`: a written report parses back to the same
  cases; in `impact.rs`, a reach equals what `finding_classes` walks.

### 9.4 Benchmark

The F5 benchmark extended: on the corpus, `jrs test` after a branch switch
that changes one class, with an empty `target/` and a warm local cache —
measured against Gradle's build cache, which caches the `test` task whole
and so reruns the full suite for the same switch.
