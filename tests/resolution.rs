//! Resolution against a `file://` repository fixture.
//!
//! Hermetic by construction: no test in this file touches the network, so CI
//! needs nothing but a Rust toolchain to run it (SPEC §10.1).

mod common;

use std::path::Path;

use common::{FixtureRepo, Scratch};
use jrs::lockfile::Lockfile;
use jrs::manifest::Manifest;
use jrs::resolve::coord::Ga;
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
