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

/// A POM with `dependencies` and a jar of `jar`, published under `root` in
/// Maven layout, checksums and all.
fn publish_into(root: &Path, coord: &Coord, dependencies: &[&str], jar: &[u8]) {
    let deps: String = dependencies
        .iter()
        .map(|gav| {
            let c = Coord::parse(gav).unwrap();
            format!(
                "<dependency><groupId>{}</groupId><artifactId>{}</artifactId>\
                 <version>{}</version></dependency>",
                c.group, c.artifact, c.version
            )
        })
        .collect();
    let pom = format!(
        "<project><groupId>{}</groupId><artifactId>{}</artifactId>\
         <version>{}</version><dependencies>{deps}</dependencies></project>",
        coord.group, coord.artifact, coord.version
    );
    for (ext, bytes) in [("pom", pom.as_bytes()), ("jar", jar)] {
        let path = root.join(coord.repo_path(ext));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(
            path.with_extension(format!("{ext}.sha1")),
            resolve::repo::sha1_hex(bytes),
        )
        .unwrap();
    }
}

fn file_names(jars: &[std::path::PathBuf]) -> Vec<String> {
    jars.iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn runtime_only_dependencies_run_and_are_tested_but_not_compiled_against() {
    let scratch = Scratch::new("resolve-runtime-only");
    let fixture = FixtureRepo::new(&scratch);
    let only = manifest(
        &fixture,
        "[dependencies]\n\"org.example:lib\" = { version = \"1.0.0\", runtime-only = true }",
    );
    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&only, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();

    // `lib` and what it brings in are never compiled against...
    assert!(resolution.classpath(Classpath::Compile).is_empty());
    // ...but run with, shipped, and on the test classpath.
    assert_eq!(resolution.runtime_classpath().len(), 2);
    assert_eq!(
        resolution.classpath(Classpath::Runtime),
        resolution.runtime_classpath()
    );
    assert_eq!(resolution.classpath(Classpath::Test).len(), 2);
    for package in &resolution.packages {
        assert_eq!(package.classpath, Classpath::Runtime, "{}", package.coord);
    }

    // The lockfile keeps them apart.
    let text = Lockfile::from_resolution(&only, &resolution).render();
    assert!(text.contains("classpath = \"runtime\""), "{text}");
    let again = Lockfile::parse(&text, Path::new("jrs.lock")).unwrap();
    assert!(again.matches(&only));
    let mut from_lock = again.to_resolution();
    resolve::locate_cached(&mut from_lock, &fetcher);
    assert!(from_lock.classpath(Classpath::Compile).is_empty());
    assert_eq!(
        from_lock.runtime_classpath(),
        resolution.runtime_classpath()
    );
}

#[test]
fn a_package_reached_several_ways_lands_on_every_classpath_that_needs_it() {
    let scratch = Scratch::new("resolve-runtime-widen");
    let fixture = FixtureRepo::new(&scratch);
    for (artifact, deps) in [
        ("r", &["w:shared:1", "w:both:1"][..]),
        ("c", &["w:shared:1"][..]),
        ("t", &["w:both:1"][..]),
        ("shared", &[][..]),
        ("both", &[][..]),
    ] {
        publish_into(
            &fixture.root,
            &Coord::new("w", artifact, "1"),
            deps,
            artifact.as_bytes(),
        );
    }
    let m = manifest(
        &fixture,
        "[dependencies]\n\"w:r\" = { version = \"1\", runtime-only = true }\n\
         \"w:c\" = { version = \"1\", compile-only = true }\n\
         [dev-dependencies]\n\"w:t\" = \"1\"",
    );
    let resolution = resolve::resolve(&m, &fixture.fetcher(), 4).unwrap();
    let of = |a: &str| resolution.get(&Ga::new("w", a)).unwrap().classpath;
    assert_eq!(of("r"), Classpath::Runtime);
    assert_eq!(of("c"), Classpath::Provided);
    assert_eq!(of("t"), Classpath::Test);
    assert_eq!(
        of("shared"),
        Classpath::Compile,
        "compiled against through `c`, run with through `r`"
    );
    assert_eq!(
        of("both"),
        Classpath::Runtime,
        "run with through `r`, and tests see everything"
    );
}

/// A project at `app/` in the scratch directory, with `body` after its
/// `[project]` table and the fixture repository after that.
fn project(scratch: &Scratch, fixture: &FixtureRepo, body: &str) -> Manifest {
    let path = scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\n{body}\n{}",
            fixture.manifest_section()
        ),
    );
    Manifest::load(path).unwrap()
}

#[test]
fn a_local_jar_is_taken_as_it_is_and_pinned_by_its_path() {
    let scratch = Scratch::new("resolve-local");
    let fixture = FixtureRepo::new(&scratch);
    scratch.write("app/libs/vendor.jar", "vendor bytes");
    let m = project(
        &scratch,
        &fixture,
        "[dependencies]\n\"org.example:lib\" = \"1.0.0\"\nvendor = { path = \"libs/vendor.jar\" }",
    );
    let fetcher = fixture.fetcher();
    let mut resolution = resolve::resolve(&m, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();

    assert_eq!(
        file_names(&resolution.classpath(Classpath::Compile)),
        ["lib-1.0.0.jar", "vendor.jar", "core-1.0.0.jar"],
        "declared coordinates, then local jars, then transitives"
    );
    assert_eq!(
        resolution.runtime_classpath(),
        resolution.classpath(Classpath::Compile)
    );
    assert!(
        resolution.roots.iter().all(|r| r.artifact != "vendor"),
        "a local jar is not a coordinate"
    );
    let pinned = resolution.local_jar("vendor").unwrap().checksum.clone();
    assert!(pinned.as_deref().unwrap().starts_with("sha256:"));

    let lock = Lockfile::from_resolution(&m, &resolution);
    let text = lock.render();
    assert!(text.contains("version = 1\n"), "{text}");
    assert!(
        text.contains(
            "\n[[local]]\nname = \"vendor\"\npath = \"libs/vendor.jar\"\nclasspath = \"compile\"\n"
        ),
        "{text}"
    );
    assert!(
        !text.contains(&scratch.path.display().to_string()),
        "no absolute paths:\n{text}"
    );

    // From the lockfile, the jar is found where the manifest says, and checked.
    let mut from_lock = lock.to_resolution();
    resolve::locate_cached(&mut from_lock, &fetcher);
    resolve::attach_local(&mut from_lock, &m.root).unwrap();
    assert_eq!(
        from_lock.classpath(Classpath::Compile),
        resolution.classpath(Classpath::Compile)
    );

    // New bytes under the same name no longer match the pin...
    scratch.write("app/libs/vendor.jar", "other bytes");
    let mut stale = lock.to_resolution();
    let error = resolve::attach_local(&mut stale, &m.root).unwrap_err();
    assert_eq!(error.exit_code(), 1);
    let message = error.to_string();
    assert!(message.contains("libs/vendor.jar"), "{message}");
    assert!(message.contains("jrs update"), "{message}");
    // ...until a fresh resolution pins them.
    let fresh = resolve::resolve(&m, &fetcher, 4).unwrap();
    assert_ne!(fresh.local_jar("vendor").unwrap().checksum, pinned);

    // A jar that is not there is a manifest error that names it.
    std::fs::remove_file(scratch.join("app/libs/vendor.jar")).unwrap();
    let error = resolve::resolve(&m, &fetcher, 4).unwrap_err();
    assert_eq!(error.exit_code(), 2);
    let message = error.to_string();
    assert!(message.contains("libs/vendor.jar"), "{message}");
    assert!(message.contains("does not exist"), "{message}");
}

#[test]
fn local_jars_take_the_classpath_their_table_and_flags_say() {
    let scratch = Scratch::new("resolve-local-classpaths");
    let fixture = FixtureRepo::new(&scratch);
    for jar in ["api", "driver", "fixtures"] {
        scratch.write(&format!("app/libs/{jar}.jar"), jar);
    }
    let m = project(
        &scratch,
        &fixture,
        "[dependencies]\ndriver = { path = \"libs/driver.jar\", runtime-only = true }\n\
         api = { path = \"libs/api.jar\", compile-only = true }\n\
         [dev-dependencies]\nfixtures = { path = \"libs/fixtures.jar\" }",
    );
    let resolution = resolve::resolve(&m, &fixture.fetcher(), 4).unwrap();
    assert!(resolution.packages.is_empty(), "nothing to walk");
    assert_eq!(
        file_names(&resolution.classpath(Classpath::Compile)),
        ["api.jar"]
    );
    assert_eq!(file_names(&resolution.runtime_classpath()), ["driver.jar"]);
    assert_eq!(
        file_names(&resolution.classpath(Classpath::Test)),
        ["api.jar", "driver.jar", "fixtures.jar"]
    );
}

#[test]
fn a_repository_with_groups_is_the_only_one_asked_for_them() {
    let scratch = Scratch::new("resolve-groups");
    let fixture = FixtureRepo::new(&scratch);
    // `internal` serves com.acme, and holds a planted org.example:lib that it
    // must never be asked for.
    let internal = scratch.join("internal");
    publish_into(
        &internal,
        &Coord::new("com.acme", "billing", "1.0"),
        &[],
        b"billing",
    );
    publish_into(
        &internal,
        &Coord::new("org.example", "lib", "1.0.0"),
        &[],
        b"planted",
    );
    // The public fixture has a com.acme artifact too, which must never come
    // from there: the dependency-confusion case.
    publish_into(
        &fixture.root,
        &Coord::new("com.acme", "impostor", "1.0"),
        &[],
        b"impostor",
    );

    let parse = |deps: &str, groups: &str| {
        let text = format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\n[dependencies]\n{deps}\n\
             [repositories]\ninternal = {{ url = \"{}\"{groups} }}\nfixture = \"{}\"\n",
            resolve::repo::file_url(&internal),
            resolve::repo::file_url(&fixture.root)
        );
        Manifest::parse(&text, Path::new("/p/jrs.toml"), Path::new("/p")).unwrap()
    };
    let groups = ", groups = [\"com.acme\", \"com.acme.*\"]";
    // Maven Central is left out, so nothing here can reach the network.
    let fetcher_for = |m: &Manifest| {
        resolve::repo::Fetcher::new(
            m.repositories
                .iter()
                .filter(|r| r.name != "central")
                .cloned()
                .collect(),
            resolve::cache::Cache::with_root(scratch.join("cache")),
            false,
        )
    };

    let m = parse(
        "\"org.example:lib\" = \"1.0.0\"\n\"com.acme:billing\" = \"1.0\"",
        groups,
    );
    let fetcher = fetcher_for(&m);
    let mut resolution = resolve::resolve(&m, &fetcher, 4).unwrap();
    resolve::fetch_jars(&mut resolution, &fetcher, 4).unwrap();
    let bytes = |artifact: &str| {
        let package = resolution
            .packages
            .iter()
            .find(|p| p.coord.artifact == artifact)
            .unwrap();
        std::fs::read(package.jar.as_ref().unwrap()).unwrap()
    };
    assert_eq!(bytes("lib"), b"jar for org.example:lib:1.0.0");
    assert_eq!(bytes("billing"), b"billing");

    let m = parse("\"com.acme:impostor\" = \"1.0\"", groups);
    let error = resolve::resolve(&m, &fetcher_for(&m), 4)
        .unwrap_err()
        .to_string();
    assert!(error.contains("looked in: internal"), "{error}");
    assert!(!error.contains("fixture"), "{error}");

    // `groups` are part of what the lockfile was resolved from.
    assert_ne!(
        jrs::lockfile::manifest_checksum(&m),
        jrs::lockfile::manifest_checksum(&parse("\"com.acme:impostor\" = \"1.0\"", ""))
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

// ---- jrs fetch and jrs metadata --------------------------------------------
//
// These drive the jrs binary, with the fixture's cache as `JRS_CACHE_DIR` and
// no user configuration, so fixture artifacts never reach the user's cache.
// Neither command compiles, so neither needs a JDK.

/// A project depending on `org.example:lib`, beside the fixture repository.
fn fetchable_project(scratch: &Scratch, fixture: &FixtureRepo) -> std::path::PathBuf {
    scratch.write(
        "app/jrs.toml",
        &format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\n\n\
             [dependencies]\n\"org.example:lib\" = \"1.0.0\"\n\n{}",
            fixture.manifest_section()
        ),
    );
    scratch.join("app")
}

/// Run the jrs binary on the project at `root`. Returns the exit code, stdout
/// and stderr.
fn jrs(
    fixture: &FixtureRepo,
    scratch: &Scratch,
    root: &Path,
    args: &[&str],
) -> (i32, String, String) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jrs"))
        .arg("--manifest-path")
        .arg(root)
        .args([
            "--progress",
            "never",
            "--color",
            "never",
            "--charset",
            "ascii",
        ])
        .args(args)
        .env("JRS_CACHE_DIR", &fixture.cache)
        .env("JRS_CONFIG", scratch.join("no-config.toml"))
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The JSON string a path prints as.
fn json_path(path: &Path) -> String {
    jrs::json::escape(&path.display().to_string())
}

/// The value of the first `"key": ` after `anchor` in `text`.
fn field_after<'a>(text: &'a str, anchor: &str, key: &str) -> &'a str {
    let start = text
        .find(anchor)
        .unwrap_or_else(|| panic!("no {anchor} in:\n{text}"));
    let rest = &text[start..];
    let at = rest
        .find(&format!("\"{key}\": "))
        .unwrap_or_else(|| panic!("no {key} after {anchor}"));
    let value = &rest[at + key.len() + 4..];
    value[..value.find('\n').unwrap_or(value.len())].trim_end_matches(',')
}

#[test]
fn fetch_sources_downloads_what_is_published_and_warns_about_the_rest() {
    let scratch = Scratch::new("fetch-sources");
    let fixture = FixtureRepo::new(&scratch);
    let lib = Coord::new("org.example", "lib", "1.0.0");
    fixture.publish_sources(&lib, b"sources of lib");
    let root = fetchable_project(&scratch, &fixture);

    let (code, stdout, stderr) = jrs(&fixture, &scratch, &root, &["fetch", "--sources"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "", "fetch prints nothing to stdout");
    assert!(stderr.contains(" Downloading 2 artifacts\n"), "{stderr}");
    assert!(stderr.contains(" Downloading 2 sources jars\n"), "{stderr}");
    assert!(
        stderr.contains("warning: 1 dependency publishes no sources jar: org.example:core:1.0.0\n"),
        "a missing sources jar is a warning: {stderr}"
    );
    assert!(
        stderr.contains("Finished fetched 2 dependencies and 1 of 2 sources jars"),
        "{stderr}"
    );

    let cache = jrs::resolve::cache::Cache::with_root(&fixture.cache);
    assert!(cache.contains(&lib, "jar"));
    assert!(cache.contains(&Coord::new("org.example", "core", "1.0.0"), "jar"));
    assert_eq!(
        std::fs::read(cache.path_for(&lib.sources(), "jar")).unwrap(),
        b"sources of lib"
    );
    assert!(
        root.join("jrs.lock").is_file(),
        "fetch pins what it resolved"
    );
    assert!(!root.join("target").exists(), "fetch never builds");

    // A second fetch finds the jars cached. The sources jar that was never
    // published is asked for again — it may have been published since —
    // and is still only a warning.
    let (code, _, stderr) = jrs(&fixture, &scratch, &root, &["fetch", "--sources"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("artifacts"), "{stderr}");
    assert!(stderr.contains(" Downloading 1 sources jars\n"), "{stderr}");
    assert!(stderr.contains("publishes no sources jar"), "{stderr}");
}

#[test]
fn fetch_sources_offline_warns_rather_than_fails() {
    let scratch = Scratch::new("fetch-sources-offline");
    let fixture = FixtureRepo::new(&scratch);
    fixture.publish_sources(&Coord::new("org.example", "lib", "1.0.0"), b"src");
    let root = fetchable_project(&scratch, &fixture);
    let (code, _, stderr) = jrs(&fixture, &scratch, &root, &["fetch"]);
    assert_eq!(code, 0, "{stderr}");

    let (code, _, stderr) = jrs(
        &fixture,
        &scratch,
        &root,
        &["--offline", "fetch", "--sources"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("2 sources jars are not in the cache, and --offline was given"),
        "{stderr}"
    );
}

#[test]
fn metadata_prints_the_model_with_classpaths_and_cached_sources() {
    let scratch = Scratch::new("metadata");
    let fixture = FixtureRepo::new(&scratch);
    let lib = Coord::new("org.example", "lib", "1.0.0");
    fixture.publish_sources(&lib, b"sources of lib");
    let root = fetchable_project(&scratch, &fixture);
    let (code, _, stderr) = jrs(&fixture, &scratch, &root, &["fetch", "--sources"]);
    assert_eq!(code, 0, "{stderr}");

    let (code, json, stderr) = jrs(&fixture, &scratch, &root, &["metadata"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(json.starts_with("{\n  \"version\": 1,\n"), "{json}");
    assert!(json.ends_with("\n}\n"), "{json}");
    assert!(
        !stderr.contains("Compiling"),
        "metadata never compiles: {stderr}"
    );
    assert!(!root.join("target").exists(), "metadata never builds");

    let root = std::path::absolute(&root).unwrap();
    assert_eq!(
        field_after(&json, "\"project\"", "root"),
        json_path(&root),
        "{json}"
    );
    assert_eq!(
        field_after(&json, "\"main\"", "output"),
        json_path(&root.join("target").join("classes"))
    );

    // Each classpath names its jars in the order the build uses: the
    // declared dependency, then what it brings in.
    let cache = jrs::resolve::cache::Cache::with_root(&fixture.cache);
    for classpath in ["\"compile\"", "\"runtime\"", "\"test\""] {
        let from = json.find(classpath).unwrap();
        let section = &json[from..];
        let lib_at = section.find("\"org.example:lib:1.0.0\"").unwrap();
        let core_at = section.find("\"org.example:core:1.0.0\"").unwrap();
        assert!(lib_at < core_at, "{classpath} is out of order:\n{json}");
    }
    let lib_entry = "\"coordinate\": \"org.example:lib:1.0.0\"";
    assert_eq!(
        field_after(&json, lib_entry, "jar"),
        json_path(&cache.path_for(&lib, "jar"))
    );
    assert_eq!(
        field_after(&json, lib_entry, "sources"),
        json_path(&cache.path_for(&lib.sources(), "jar"))
    );
    assert_eq!(
        field_after(
            &json,
            "\"coordinate\": \"org.example:core:1.0.0\"",
            "sources"
        ),
        "null",
        "core publishes no sources jar"
    );
}

#[test]
fn metadata_without_dependencies_resolves_nothing() {
    let scratch = Scratch::new("metadata-no-deps");
    let fixture = FixtureRepo::new(&scratch);
    let root = fetchable_project(&scratch, &fixture);

    let (code, json, stderr) = jrs(&fixture, &scratch, &root, &["metadata", "--no-deps"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(json.contains("\n  \"classpaths\": null,\n"), "{json}");
    assert!(!stderr.contains("Resolving"), "{stderr}");
    assert!(!root.join("jrs.lock").exists(), "nothing was resolved");
    assert!(!fixture.cache.exists(), "nothing was downloaded");
}
