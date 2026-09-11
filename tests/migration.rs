//! Migration fixtures, each paired with the manifest it must produce.
//!
//! The comparison is exact, so a change in translation shows up as a diff in a
//! reviewable file rather than as a changed assertion (SPEC §11.5). The version
//! header is checked separately: it carries the jrs version, which would make the
//! fixture stale on every release.

mod common;

use std::path::{Path, PathBuf};

use common::{FixtureRepo, Scratch, copy_dir, fixtures};
use jrs::manifest::Manifest;
use jrs::migrate::{self, Migration, Source};
use jrs::resolve::{self, Classpath};

fn migrate_fixture(name: &str) -> (Migration, PathBuf) {
    let dir = fixtures().join("migrate").join(name);
    (migrate::plan(&dir, None).unwrap(), dir)
}

fn expected(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("expected-jrs.toml")).unwrap()
}

#[test]
fn the_maven_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("maven");
    assert_eq!(migration.source, Source::Maven);
    assert_eq!(migration.manifest.render(None), expected(&dir));
}

#[test]
fn the_gradle_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle");
    assert_eq!(migration.source, Source::Gradle);
    assert_eq!(migration.manifest.render(None), expected(&dir));
}

/// The long form in use: classifiers, exclusions, compile-only, a test-jar,
/// annotation processors and the test JVM's arguments.
#[test]
fn the_maven_extras_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("maven-extras");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    assert!(
        migration.report.not_migrated.is_empty(),
        "{:?}",
        migration.report.not_migrated
    );
    let review = migration.report.needs_review.join("\n");
    assert!(
        review.contains("legacy-native` = lib/native.jar, compile-only"),
        "{review}"
    );
    assert!(review.contains("annotation processors"), "{review}");
}

/// start.spring.io's Gradle builds, in both DSLs, taken unchanged: the Boot
/// plugins become Boot's BOM in [managed], the starters stay versionless, and
/// the @SpringBootApplication class is the main class.
#[test]
fn the_spring_boot_gradle_fixtures_translate_exactly() {
    for name in ["spring-boot-gradle", "spring-boot-kts"] {
        let (migration, dir) = migrate_fixture(name);
        assert_eq!(migration.source, Source::Gradle);
        assert_eq!(migration.manifest.render(None), expected(&dir), "{name}");
        assert!(
            migration.report.not_migrated.is_empty(),
            "{name}: {:?}",
            migration.report.not_migrated
        );
        let migrated = migration.report.migrated.join("\n");
        assert!(
            migrated.contains("spring-boot-dependencies = 4.1.1, a BOM"),
            "{migrated}"
        );
        assert!(
            migrated.contains("(the @SpringBootApplication class)"),
            "{migrated}"
        );
        let review = migration.report.needs_review.join("\n");
        assert!(review.contains("`jrs package --fat`"), "{review}");
    }
}

/// start.spring.io's Maven build: the starter parent stands for Boot's BOM.
#[test]
fn the_spring_boot_maven_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("spring-boot-maven");
    assert_eq!(migration.manifest.render(None), expected(&dir));
    assert!(
        migration.report.not_migrated.is_empty(),
        "{:?}",
        migration.report.not_migrated
    );
    let migrated = migration.report.migrated.join("\n");
    assert!(
        migrated.contains("(from <parent> spring-boot-starter-parent)"),
        "{migrated}"
    );
    assert!(migrated.contains("from java.version"), "{migrated}");
}

/// exec-maven-plugin executions as tasks and hooks, `<dependencyManagement>`
/// as [managed], and a system-scoped jar as a local one. The jar is made here,
/// as an empty file, since nothing binary is committed.
#[test]
fn the_maven_exec_fixture_translates_exactly() {
    let scratch = Scratch::new("migrate-maven-exec");
    let dir = scratch.join("project");
    copy_dir(&fixtures().join("migrate/maven-exec"), &dir);
    scratch.write("project/lib/native.jar", "");

    let migration = migrate::plan(&dir, None).unwrap();
    assert_eq!(migration.manifest.render(None), expected(&dir));
    let parsed =
        Manifest::parse(&migration.render_manifest(), &dir.join("jrs.toml"), &dir).unwrap();
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);

    let migrated = migration.report.migrated.join("\n");
    assert!(
        migrated.contains("`generate-parser` → [tasks.generate-parser], run by hooks.pre-compile"),
        "{migrated}"
    );
    assert!(
        migrated.contains("1 version from <dependencyManagement>"),
        "{migrated}"
    );
    let review = migration.report.needs_review.join("\n");
    assert!(
        review.contains("[tasks.generate-parser] — if it generates sources"),
        "{review}"
    );
    assert!(!review.contains("not there yet"), "{review}");
    let skipped = migration.report.not_migrated.join("\n");
    assert!(
        skipped.contains("`publish-docs` — its phase `deploy`"),
        "{skipped}"
    );
    assert!(skipped.contains("`start-db` — <async>"), "{skipped}");
    assert!(skipped.contains("com.sun:tools"), "{skipped}");
}

/// The Kotlin DSL, with exclusion closures, `isTransitive = false`, a classified
/// coordinate, compile-only, and JVM arguments for both run and test.
#[test]
fn the_gradle_kotlin_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle-kts");
    assert_eq!(migration.source, Source::Gradle);
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let review = migration.report.needs_review.join("\n");
    assert!(
        review.contains("JDK 17 does not accept"),
        "a Java 17 project must not get -proc:full: {review}"
    );
}

/// kotlin-maven-plugin at `${kotlin.version}`: the stdlib it implies is left
/// out, `<jvmTarget>` becomes java.source, `src/main/kotlin` stays Kotlin's
/// own root, and the Spring compiler plugin is reported.
#[test]
fn the_maven_kotlin_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("maven-kotlin");
    assert_eq!(migration.source, Source::Maven);
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let migrated = migration.report.migrated.join("\n");
    assert!(migrated.contains("[kotlin] version = 2.2.0"), "{migrated}");
    assert!(
        migrated.contains("org.jetbrains.kotlin:kotlin-stdlib:2.2.0 — left out"),
        "{migrated}"
    );
    assert!(migrated.contains("<jvmTarget>"), "{migrated}");
    assert!(
        migrated.contains("src/main/kotlin is [kotlin]'s own root"),
        "{migrated}"
    );

    let skipped = migration.report.not_migrated.join("\n");
    assert!(skipped.contains("`spring`"), "{skipped}");
    assert!(
        skipped.contains("compiler plugins are not supported yet"),
        "{skipped}"
    );
    assert!(!skipped.contains("no plugin system"), "{skipped}");
}

/// scala-maven-plugin's `<scalaVersion>`, with the Scala 3 library it implies
/// declared by hand beside it.
#[test]
fn the_maven_scala_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("maven-scala");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let migrated = migration.report.migrated.join("\n");
    assert!(migrated.contains("[scala] version = 3.7.1"), "{migrated}");
    assert!(
        migrated.contains("org.scala-lang:scala3-library_3:3.7.1 — left out"),
        "{migrated}"
    );
    let skipped = migration.report.not_migrated.join("\n");
    assert!(!skipped.contains("no plugin system"), "{skipped}");
}

/// `kotlin("jvm")`, `kotlin("...")` modules, `jvmToolchain` and a Kotlin
/// compiler plugin, in the Kotlin DSL.
#[test]
fn the_gradle_kotlin_language_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle-kotlin");
    assert_eq!(migration.source, Source::Gradle);
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let migrated = migration.report.migrated.join("\n");
    assert!(migrated.contains("java.jdk = 21"), "{migrated}");
    assert!(
        migrated.contains("org.jetbrains.kotlin:kotlin-stdlib:2.2.0 — left out"),
        "{migrated}"
    );
    let review = migration.report.needs_review.join("\n");
    assert!(review.contains("kotlin-test-junit5"), "{review}");
    assert!(review.contains("capability"), "{review}");

    let skipped = migration.report.not_migrated.join("\n");
    assert!(skipped.contains("plugin.spring"), "{skipped}");
    assert!(
        skipped.contains("compiler plugins are not supported yet"),
        "{skipped}"
    );
    assert!(!skipped.contains("no plugin system"), "{skipped}");
}

/// Groovy for Spock tests only: the version comes from the test dependency,
/// and that dependency stays, since it is what keeps Groovy off the runtime.
#[test]
fn the_gradle_groovy_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle-groovy");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let migrated = migration.report.migrated.join("\n");
    assert!(migrated.contains("[groovy] version = 4.0.27"), "{migrated}");
    assert!(!migrated.contains("left out"), "{migrated}");
    let skipped = migration.report.not_migrated.join("\n");
    assert!(!skipped.contains("no plugin system"), "{skipped}");
}

/// Exec, JavaExec and aggregate tasks become [tasks]; `compileJava.dependsOn`
/// and `jar.finalizedBy` become hooks; the rest is reported, task by task.
#[test]
fn the_gradle_tasks_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle-tasks");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let skipped = migration.report.not_migrated.join("\n");
    assert!(
        skipped.contains("task `printVersion` — `doLast { }`"),
        "{skipped}"
    );
    assert!(
        skipped.contains("task `copyDocs` — its type `Copy`"),
        "{skipped}"
    );
    assert!(
        skipped.contains("task `deploy` — it depends on `copyDocs`"),
        "{skipped}"
    );
    assert!(
        skipped.contains("`build.dependsOn 'printVersion'`"),
        "{skipped}"
    );

    let review = migration.report.needs_review.join("\n");
    assert!(review.contains("Gradle's output directory"), "{review}");
    assert!(review.contains("`jrs task checksum` alone"), "{review}");
}

/// `environment` and `workingDir` on Gradle's `run` and `test`, and Mockito's
/// agent recipe, become `run.env`, `run.cwd`, `test.env` and
/// `test.java-agents`; what is not a literal is reported.
#[test]
fn the_gradle_environment_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle-env");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let skipped = migration.report.not_migrated.join("\n");
    assert!(skipped.contains("HOME_DIR"), "{skipped}");
    let migrated = migration.report.migrated.join("\n");
    assert!(migrated.contains("test.java-agents"), "{migrated}");
    assert!(migrated.contains("run.cwd"), "{migrated}");
}

/// `jar { manifest { attributes(...) } }`: literal values and the project's
/// version carried over in order, `Main-Class` as the main class, and what
/// cannot be — a computed value, an attribute jrs writes, `withSourcesJar()` —
/// reported.
#[test]
fn the_gradle_jar_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle-jar");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let skipped = migration.report.not_migrated.join("\n");
    assert!(
        skipped.contains("`Built-By` — its value is computed"),
        "{skipped}"
    );
    assert!(skipped.contains("`Class-Path` belongs to jrs"), "{skipped}");
    assert!(skipped.contains("`jrs package --sources`"), "{skipped}");
    assert!(skipped.contains("`jrs package --javadoc`"), "{skipped}");
    let migrated = migration.report.migrated.join("\n");
    assert!(
        migrated.contains("project.main-class = com.example.JarDemo (from the jar manifest)"),
        "{migrated}"
    );
}

/// `files()` and `fileTree()` as local jars, `runtimeOnly`, and `content` /
/// `exclusiveContent` as `groups`. The jars are made here, as empty files,
/// since migration only lists them and nothing binary is committed.
#[test]
fn the_gradle_local_fixture_translates_exactly() {
    let scratch = Scratch::new("migrate-gradle-local");
    let dir = scratch.join("project");
    copy_dir(&fixtures().join("migrate/gradle-local"), &dir);
    for file in [
        "libs/ojdbc11.jar",
        "libs/vendor-api.jar",
        "drivers/h2-extra.jar",
        "drivers/mysql-connector.jar",
        "drivers/README.txt",
        "test-libs/fixtures.jar",
    ] {
        scratch.write(&format!("project/{file}"), "");
    }

    let migration = migrate::plan(&dir, None).unwrap();
    assert_eq!(migration.manifest.render(None), expected(&dir));
    let parsed =
        Manifest::parse(&migration.render_manifest(), &dir.join("jrs.toml"), &dir).unwrap();
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);

    let review = migration.report.needs_review.join("\n");
    assert!(
        review.contains("expanded to the 2 jars in drivers/"),
        "{review}"
    );
    assert!(review.contains("repository `acme`"), "{review}");
    assert!(
        !review.contains("jitpack"),
        "exclusiveContent is exact: {review}"
    );
    let migrated = migration.report.migrated.join("\n");
    assert!(
        migrated.contains("org.slf4j:slf4j-api (runtimeOnly, merged"),
        "{migrated}"
    );
    assert!(
        migration.report.not_migrated.is_empty(),
        "{:?}",
        migration.report.not_migrated
    );
}

/// start.spring.io's Kotlin build, taken unchanged: Boot's BOM, the
/// `freeCompilerArgs` as kotlinc's, `DemoApplicationKt` as the main class —
/// and the `spring` compiler plugin reported, since without it Spring cannot
/// proxy Kotlin's final classes (JVM_LANGUAGES.md §14.2).
#[test]
fn the_spring_boot_kotlin_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("spring-boot-kotlin");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let migrated = migration.report.migrated.join("\n");
    assert!(
        migrated.contains(
            "project.main-class = com.example.demo.DemoApplicationKt (the @SpringBootApplication \
             class)"
        ),
        "{migrated}"
    );
    assert!(
        migrated.contains("[kotlin] kotlinc-args = [\"-Xjsr305=strict\""),
        "{migrated}"
    );
    let skipped = &migration.report.not_migrated;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(skipped[0].contains("plugin.spring"), "{skipped:?}");
    assert!(skipped[0].contains("Spring's proxies"), "{skipped:?}");
}

/// `maxParallelForks` becomes `test.forks`; `forkEvery` is reported, since
/// jrs never restarts a test JVM part of the way through its share.
#[test]
fn the_gradle_forks_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("gradle-forks");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let migrated = migration.report.migrated.join("\n");
    assert!(
        migrated.contains("test.forks = 4 (from maxParallelForks)"),
        "{migrated}"
    );
    let skipped = &migration.report.not_migrated;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(
        skipped[0].starts_with("`forkEvery = 100` — Gradle restarts a test JVM"),
        "{skipped:?}"
    );
}

/// Profiles as a plain `mvn` build picks them: the default one merged in —
/// its property, its dependency and its surefire settings, merged over the
/// project's own — and the ones waiting on `-P`, a property or the JDK
/// listed. surefire's `<forkCount>` becomes `test.forks`.
#[test]
fn the_maven_profiles_fixture_translates_exactly() {
    let (migration, dir) = migrate_fixture("maven-profiles");
    assert_eq!(migration.manifest.render(None), expected(&dir));

    let migrated = migration.report.migrated.join("\n");
    assert!(
        migrated.contains("<profile> postgres — active by default in a plain `mvn` build"),
        "{migrated}"
    );
    assert!(
        migrated.contains("test.forks = 3 (from surefire <forkCount>)"),
        "{migrated}"
    );
    let review = migration.report.needs_review.join("\n");
    assert!(
        review.contains("Maven drops it whenever `release`"),
        "{review}"
    );
    let skipped = migration.report.not_migrated.join("\n");
    for id in ["h2", "release", "jdk25"] {
        assert!(skipped.contains(&format!("<profile> {id} —")), "{skipped}");
    }
    assert!(
        !skipped.contains("maven-gpg-plugin"),
        "a profile left out does not put its plugins in the build: {skipped}"
    );
}

#[test]
fn every_expected_manifest_is_a_manifest_jrs_can_read() {
    for name in [
        "maven",
        "gradle",
        "maven-extras",
        "gradle-kts",
        "maven-kotlin",
        "maven-scala",
        "gradle-kotlin",
        "gradle-groovy",
        "gradle-tasks",
        "gradle-env",
        "gradle-jar",
        "spring-boot-gradle",
        "spring-boot-kts",
        "spring-boot-maven",
        "spring-boot-kotlin",
        "maven-exec",
        "maven-profiles",
        "gradle-forks",
    ] {
        let (migration, dir) = migrate_fixture(name);
        let text = migration.render_manifest();
        let parsed = Manifest::parse(&text, &dir.join("jrs.toml"), &dir).unwrap();
        assert!(
            parsed.warnings.is_empty(),
            "{name} produced a manifest with warnings: {:?}",
            parsed.warnings
        );
        assert!(text.starts_with("# Generated by jrs "));
    }
}

#[test]
fn the_maven_report_names_what_it_could_not_translate() {
    let (migration, _) = migrate_fixture("maven");
    let skipped = migration.report.not_migrated.join("\n");

    // Each of these is in the fixture precisely so the report is exercised.
    assert!(skipped.contains("lombok"), "{skipped}");
    assert!(skipped.contains("optional"), "{skipped}");
    assert!(skipped.contains("maven-antrun-plugin"), "{skipped}");
    assert!(skipped.contains("ci"), "the inactive profile: {skipped}");

    let review = migration.report.needs_review.join("\n");
    assert!(review.contains("clamped to 1.2"), "{review}");

    // ...and the understood plugins are not complained about.
    assert!(!skipped.contains("maven-jar-plugin"), "{skipped}");
    assert!(!skipped.contains("maven-compiler-plugin"), "{skipped}");
}

#[test]
fn the_gradle_report_opens_by_admitting_it_is_approximate() {
    let (migration, _) = migrate_fixture("gradle");
    let preamble = migration.report.preamble.as_deref().unwrap();
    assert!(preamble.contains("approximate"), "{preamble}");
    assert!(preamble.contains("review"), "{preamble}");

    let review = migration.report.needs_review.join("\n");
    assert!(review.contains("annotation processors"), "{review}");

    let skipped = migration.report.not_migrated.join("\n");
    assert!(skipped.contains("sharedVersion"), "{skipped}");
    assert!(skipped.contains("does.not.exist"), "{skipped}");
    assert!(skipped.contains("core, web"), "{skipped}");
    assert!(skipped.contains("checkstyle"), "{skipped}");
    assert!(skipped.contains("task `customThing`"), "{skipped}");
}

#[test]
fn migration_writes_one_file_and_touches_nothing_else() {
    let scratch = Scratch::new("migrate-write");
    let dir = scratch.join("project");
    copy_dir(&fixtures().join("migrate/maven"), &dir);
    let before = std::fs::read_to_string(dir.join("pom.xml")).unwrap();

    let migration = migrate::plan(&dir, None).unwrap();
    let written = migrate::write(&migration, &dir, false).unwrap().unwrap();

    assert_eq!(written, dir.join("jrs.toml"));
    assert_eq!(
        std::fs::read_to_string(dir.join("pom.xml")).unwrap(),
        before,
        "the original build file must survive untouched"
    );

    // Refuses to clobber, unless told to.
    let error = migrate::write(&migration, &dir, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--force"), "{error}");
    migrate::write(&migration, &dir, true).unwrap();
}

/// The round trip SPEC §11.5 asks for: migrate, then build, and check that the
/// resolved classpath matches a checked expectation.
#[test]
fn a_migrated_project_resolves_the_classpath_it_should() {
    let scratch = Scratch::new("migrate-round-trip");
    let fixture = FixtureRepo::new(&scratch);
    let dir = scratch.join("project");
    std::fs::create_dir_all(&dir).unwrap();

    // A pom.xml over the same coordinates the local repository fixture serves,
    // so the round trip stays hermetic.
    std::fs::write(
        dir.join("pom.xml"),
        format!(
            r#"<project>
  <groupId>org.example</groupId>
  <artifactId>migrated</artifactId>
  <version>1.0.0</version>
  <properties><maven.compiler.release>17</maven.compiler.release></properties>
  <dependencies>
    <dependency>
      <groupId>org.example</groupId><artifactId>lib</artifactId><version>1.0.0</version>
    </dependency>
  </dependencies>
  <repositories>
    <repository><id>fixture</id><url>{}</url></repository>
  </repositories>
</project>"#,
            jrs::resolve::repo::file_url(&fixture.root)
        ),
    )
    .unwrap();

    let migration = migrate::plan(&dir, None).unwrap();
    migrate::write(&migration, &dir, false).unwrap();

    let manifest = Manifest::load(dir.join("jrs.toml")).unwrap();
    assert_eq!(manifest.name, "migrated");
    assert_eq!(manifest.java.source, Some(17));

    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&manifest, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();

    let names: Vec<String> = resolution
        .classpath(Classpath::Compile)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["lib-1.0.0.jar", "core-1.0.0.jar"],
        "direct dependency first, then its transitive"
    );
}

#[test]
fn detection_reports_what_it_looked_for() {
    let scratch = Scratch::new("migrate-detect");
    let empty = scratch.join("empty");
    std::fs::create_dir_all(&empty).unwrap();

    let error = migrate::plan(&empty, None).unwrap_err();
    assert_eq!(error.exit_code(), 2, "detection failure is a usage error");
    let message = error.to_string();
    assert!(message.contains("pom.xml"), "{message}");
    assert!(message.contains("build.gradle"), "{message}");
}
