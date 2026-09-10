//! Resolution against a `file://` repository fixture.
//!
//! Hermetic by construction: no test in this file touches the network, so CI
//! needs nothing but a Rust toolchain to run it (SPEC §10.1).

mod common;

use std::path::Path;

use common::{FixtureRepo, Scratch};
use jrs::lockfile::Lockfile;
use jrs::manifest::Manifest;
use jrs::resolve::coord::{Coord, Ga};
use jrs::resolve::{self, Classpath};

fn manifest(fixture: &FixtureRepo, body: &str) -> Manifest {
    let text = format!(
        "[project]\nname = \"app\"\nversion = \"1.0.0\"\n{body}\n{}",
        fixture.manifest_section()
    );
    Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
}

fn names(resolution: &resolve::Resolution) -> Vec<String> {
    let mut v: Vec<String> = resolution
        .packages
        .iter()
        .map(|p| p.coord.to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn a_transitive_graph_resolves_from_a_local_repository() {
    let scratch = Scratch::new("resolve-transitive");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:lib\" = \"1.0.0\"");

    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&manifest, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();

    assert_eq!(
        names(&resolution),
        vec!["org.example:core:1.0.0", "org.example:lib:1.0.0"],
        "the test-scoped dependency of `lib` must not be walked"
    );
    assert_eq!(resolution.classpath(Classpath::Compile).len(), 2);
    for package in &resolution.packages {
        assert!(
            package.jar.as_ref().is_some_and(|p| p.is_file()),
            "{} has no jar on disk",
            package.coord
        );
        assert!(package.checksum.is_some());
    }
}

#[test]
fn a_cyclic_graph_terminates() {
    // The fixture is deliberately cyclic: lib -> core -> lib.
    let scratch = Scratch::new("resolve-cycle");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:core\" = \"1.0.0\"");
    let resolution = resolve::resolve(&manifest, &fixture.fetcher(), 4).unwrap();
    assert_eq!(
        names(&resolution),
        vec!["org.example:core:1.0.0", "org.example:lib:1.0.0"]
    );
}

#[test]
fn a_parent_pom_supplies_the_version_the_child_omits() {
    // `core` declares `lib` with no version; `app-parent` manages it to 1.0.0
    // through the `${lib.version}` property.
    let scratch = Scratch::new("resolve-parent");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:core\" = \"1.0.0\"");
    let resolution = resolve::resolve(&manifest, &fixture.fetcher(), 4).unwrap();
    assert_eq!(
        resolution
            .get(&Ga::new("org.example", "lib"))
            .unwrap()
            .coord
            .version,
        "1.0.0"
    );
}

#[test]
fn a_shallower_declaration_wins_the_conflict() {
    let scratch = Scratch::new("resolve-mediation");
    let fixture = FixtureRepo::new(&scratch);
    // The manifest asks for lib 2.0.0 directly; core drags in lib 1.0.0.
    let manifest = manifest(
        &fixture,
        "[dependencies]\n\"org.example:core\" = \"1.0.0\"\n\"org.example:lib\" = \"2.0.0\"",
    );
    let resolution = resolve::resolve(&manifest, &fixture.fetcher(), 4).unwrap();
    let lib = resolution.get(&Ga::new("org.example", "lib")).unwrap();
    assert_eq!(lib.coord.version, "2.0.0");
    assert!(lib.mediated);
    assert!(
        resolution
            .warnings
            .iter()
            .any(|w| w.contains("nearest-wins")),
        "the conflict should be reported: {:?}",
        resolution.warnings
    );
}

#[test]
fn the_lockfile_round_trips_through_disk() {
    let scratch = Scratch::new("resolve-lockfile");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:lib\" = \"1.0.0\"");

    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&manifest, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();

    let path = scratch.join("jrs.lock");
    Lockfile::from_resolution(&manifest, &resolution)
        .write(&path)
        .unwrap();

    let loaded = Lockfile::load(&path).unwrap().unwrap();
    assert!(loaded.matches(&manifest));

    let mut from_lock = loaded.to_resolution();
    resolve::locate_cached(&mut from_lock, &fetcher);
    assert_eq!(
        from_lock.classpath(Classpath::Compile),
        resolution.classpath(Classpath::Compile),
        "a lockfile must reproduce the classpath exactly"
    );
}

#[test]
fn a_second_resolution_needs_no_repository_at_all() {
    let scratch = Scratch::new("resolve-offline");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:lib\" = \"1.0.0\"");

    let mut warm = resolve::resolve(&manifest, &fixture.fetcher(), 4).unwrap();
    resolve::fetch_jars(&mut warm, &fixture.fetcher(), 4).unwrap();

    // With the cache warm, `--offline` resolves the same graph.
    let offline = fixture.offline_fetcher();
    let mut cold = resolve::resolve(&manifest, &offline, 4).unwrap();
    resolve::fetch_jars(&mut cold, &offline, 4).unwrap();
    assert_eq!(names(&cold), names(&warm));
}

#[test]
fn offline_with_an_empty_cache_fails_and_says_why() {
    let scratch = Scratch::new("resolve-offline-cold");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:lib\" = \"1.0.0\"");

    let error = resolve::resolve(&manifest, &fixture.offline_fetcher(), 4)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--offline"), "{error}");
    assert!(error.contains("not in the local cache"), "{error}");
}

#[test]
fn a_corrupt_artifact_is_refused_and_not_cached() {
    let scratch = Scratch::new("resolve-corrupt");
    let fixture = FixtureRepo::new(&scratch);

    // Rewrite the jar without updating its published checksum.
    let coord = jrs::resolve::coord::Coord::new("org.example", "lib", "1.0.0");
    std::fs::write(fixture.root.join(coord.repo_path("jar")), b"tampered").unwrap();

    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:lib\" = \"1.0.0\"");
    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&manifest, &fetcher, 4).unwrap();
    let error = resolve::fetch_jars(&mut resolution, &fetcher, 4)
        .unwrap_err()
        .to_string();

    assert!(error.contains("checksum mismatch"), "{error}");
    assert!(
        !fetcher.cache().contains(&coord, "jar"),
        "a jar that failed verification must not be left in the cache"
    );
}

#[test]
fn a_republished_jar_is_refused_against_the_lockfile() {
    let scratch = Scratch::new("resolve-pinned");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(&fixture, "[dependencies]\n\"org.example:lib\" = \"1.0.0\"");

    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&manifest, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();
    let lock = Lockfile::from_resolution(&manifest, &resolution);

    // The same coordinate is republished with different bytes — and a checksum
    // that matches them, so only the lockfile can tell — and the cache is cold.
    let coord = jrs::resolve::coord::Coord::new("org.example", "lib", "1.0.0");
    fixture.publish_jar(&coord, b"different bytes, same version");
    std::fs::remove_dir_all(&fixture.cache).unwrap();

    let fetcher = fixture.fetcher();
    let mut from_lock = lock.to_resolution();
    let error = resolve::fetch_jars(&mut from_lock, &fetcher, 4)
        .unwrap_err()
        .to_string();
    assert!(error.contains("against jrs.lock"), "{error}");
    assert!(!fetcher.cache().contains(&coord, "jar"));
}

#[test]
fn a_manifest_exclusion_prunes_the_transitive_graph() {
    let scratch = Scratch::new("resolve-manifest-exclusion");
    let fixture = FixtureRepo::new(&scratch);
    let excluded = manifest(
        &fixture,
        "[dependencies]\n\"org.example:lib\" = { version = \"1.0.0\", exclusions = [\"org.example:core\"] }",
    );
    let resolution = resolve::resolve(&excluded, &fixture.fetcher(), 4).unwrap();
    assert_eq!(names(&resolution), vec!["org.example:lib:1.0.0"]);

    let wildcard = manifest(
        &fixture,
        "[dependencies]\n\"org.example:lib\" = { version = \"1.0.0\", exclusions = [\"*:*\"] }",
    );
    let resolution = resolve::resolve(&wildcard, &fixture.fetcher(), 4).unwrap();
    assert_eq!(names(&resolution), vec!["org.example:lib:1.0.0"]);
}

#[test]
fn compile_only_dependencies_stay_off_the_runtime_classpath() {
    let scratch = Scratch::new("resolve-compile-only");
    let fixture = FixtureRepo::new(&scratch);
    let only = manifest(
        &fixture,
        "[dependencies]\n\"org.example:lib\" = { version = \"1.0.0\", compile-only = true }",
    );
    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&only, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();

    // `lib` and what it brings in are compiled against, and tested against...
    assert_eq!(resolution.classpath(Classpath::Compile).len(), 2);
    assert_eq!(resolution.classpath(Classpath::Test).len(), 2);
    // ...but never shipped or run with.
    assert!(resolution.runtime_classpath().is_empty());
    for package in &resolution.packages {
        assert_eq!(package.classpath, Classpath::Provided, "{}", package.coord);
    }

    // Reached from ordinary compile code too, `core` is needed at runtime after all.
    let both = manifest(
        &fixture,
        "[dependencies]\n\"org.example:lib\" = { version = \"1.0.0\", compile-only = true }\n\
         \"org.example:core\" = \"1.0.0\"",
    );
    let mut resolution = resolve::resolve(&both, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();
    let runtime: Vec<String> = resolution
        .runtime_classpath()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        runtime.contains(&"core-1.0.0.jar".to_string()),
        "{runtime:?}"
    );
}

#[test]
fn a_classified_artifact_resolves_beside_the_main_one() {
    let scratch = Scratch::new("resolve-classifier");
    let fixture = FixtureRepo::new(&scratch);
    let natives = Coord::new("org.example", "lib", "1.0.0").with_classifier(Some("natives".into()));
    fixture.publish_jar(&natives, b"native code");

    let manifest = manifest(
        &fixture,
        "[dependencies]\n\"org.example:lib\" = \"1.0.0\"\n\"org.example:lib:natives\" = \"1.0.0\"",
    );
    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&manifest, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();

    let jars: Vec<String> = resolution
        .classpath(Classpath::Compile)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        jars,
        vec!["lib-1.0.0.jar", "lib-1.0.0-natives.jar", "core-1.0.0.jar"],
        "both artifacts of one coordinate, and the POM's dependencies once"
    );

    // The lockfile keeps them apart.
    let path = scratch.join("jrs.lock");
    Lockfile::from_resolution(&manifest, &resolution)
        .write(&path)
        .unwrap();
    let mut again = Lockfile::load(&path).unwrap().unwrap().to_resolution();
    resolve::locate_cached(&mut again, &fetcher);
    assert_eq!(
        again.classpath(Classpath::Compile),
        resolution.classpath(Classpath::Compile)
    );
}

#[test]
fn a_compile_path_widens_what_a_test_path_already_walked() {
    // testing -> shared -> leaf, walked first as test-only at depth 2;
    // a -> b -> shared reaches `shared` from compile code a level later.
    // `leaf` has to follow `shared` onto the compile classpath, or the program
    // would run without it.
    let scratch = Scratch::new("resolve-widen");
    let fixture = FixtureRepo::new(&scratch);
    let pom = |artifact: &str, deps: &[&str]| {
        let deps: String = deps
            .iter()
            .map(|d| {
                format!(
                    "<dependency><groupId>w</groupId><artifactId>{d}</artifactId>\
                     <version>1</version></dependency>"
                )
            })
            .collect();
        let coord = Coord::new("w", artifact, "1");
        fixture.publish_pom(
            &coord,
            &format!(
                "<project><groupId>w</groupId><artifactId>{artifact}</artifactId>\
                 <version>1</version><dependencies>{deps}</dependencies></project>"
            ),
        );
        fixture.publish_jar(&coord, artifact.as_bytes());
    };
    pom("testing", &["shared"]);
    pom("shared", &["leaf"]);
    pom("leaf", &[]);
    pom("a", &["b"]);
    pom("b", &["shared"]);

    let manifest = manifest(
        &fixture,
        "[dependencies]\n\"w:a\" = \"1\"\n[dev-dependencies]\n\"w:testing\" = \"1\"",
    );
    let resolution = resolve::resolve(&manifest, &fixture.fetcher(), 4).unwrap();
    for artifact in ["a", "b", "shared", "leaf"] {
        assert_eq!(
            resolution.get(&Ga::new("w", artifact)).unwrap().classpath,
            Classpath::Compile,
            "{artifact}"
        );
    }
    assert_eq!(
        resolution.get(&Ga::new("w", "testing")).unwrap().classpath,
        Classpath::Test
    );
}

#[test]
fn resolution_is_deterministic_across_runs() {
    let scratch = Scratch::new("resolve-deterministic");
    let fixture = FixtureRepo::new(&scratch);
    let manifest = manifest(
        &fixture,
        "[dependencies]\n\"org.example:lib\" = \"1.0.0\"\n\"org.example:core\" = \"1.0.0\"",
    );

    let first = {
        let fetcher = fixture.fetcher();
        let mut r = resolve::resolve(&manifest, &fetcher, 4).unwrap();
        resolve::fetch_jars(&mut r, &fetcher, 4).unwrap();
        Lockfile::from_resolution(&manifest, &r).render()
    };
    let second = {
        let fetcher = fixture.fetcher();
        let mut r = resolve::resolve(&manifest, &fetcher, 1).unwrap();
        resolve::fetch_jars(&mut r, &fetcher, 1).unwrap();
        Lockfile::from_resolution(&manifest, &r).render()
    };
    assert_eq!(
        first, second,
        "the job count must not change what resolution produces"
    );
}
