//! Workspaces end to end (`specs/WORKSPACES.md`): members built in order
//! against each other's classes, one lockfile for all of them, compile
//! avoidance across the module boundary, and the commands that narrow to a
//! member. Each test drives the jrs binary with a cache of its own, so the
//! fixture repository's artifacts never reach the user's.

mod common;

use std::path::Path;

use common::{FixtureRepo, Scratch};

/// Run the jrs binary against `root` with a cache in `scratch`.
fn jrs(scratch: &Scratch, root: &Path, args: &[&str]) -> (i32, String, String) {
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
        .env("JRS_CACHE_DIR", scratch.join("jrs-cache"))
        .env("JRS_CONFIG", scratch.join("no-config.toml"))
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Which line of `text` first contains `needle`.
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{text}"))
}

/// A workspace of three: `core`, a library; `app`, an application on it;
/// and `tool`, which depends on nothing. Listed out of build order.
fn workspace(scratch: &Scratch) -> std::path::PathBuf {
    scratch.write(
        "ws/jrs.toml",
        "[workspace]\nmembers = [\"app\", \"core\", \"tool\"]\n\n[java]\nsource = 17\n",
    );
    scratch.write(
        "ws/core/jrs.toml",
        "[project]\nname = \"core\"\nversion = \"1.0.0\"\n",
    );
    scratch.write(
        "ws/core/src/main/java/demo/core/Greeter.java",
        "package demo.core;\n\npublic class Greeter {\n    public static String greet(String name) {\n        return \"Hello, \" + name;\n    }\n}\n",
    );
    scratch.write(
        "ws/core/src/main/resources/core.properties",
        "greeting=hello\n",
    );
    scratch.write(
        "ws/app/jrs.toml",
        "[project]\nname = \"app\"\nversion = \"2.0.0\"\nmain-class = \"demo.app.Main\"\n\n\
         [dependencies]\ncore = { path = \"../core\" }\n",
    );
    scratch.write(
        "ws/app/src/main/java/demo/app/Main.java",
        "package demo.app;\n\npublic class Main {\n    public static void main(String[] args) {\n        System.out.println(demo.core.Greeter.greet(args.length > 0 ? args[0] : \"world\"));\n    }\n}\n",
    );
    scratch.write(
        "ws/tool/jrs.toml",
        "[project]\nname = \"tool\"\nversion = \"1.0.0\"\n",
    );
    scratch.write(
        "ws/tool/src/main/java/demo/tool/Tool.java",
        "package demo.tool;\n\npublic class Tool {}\n",
    );
    scratch.join("ws")
}

#[test]
fn members_build_in_dependency_order_and_the_application_runs() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-build");
    let root = workspace(&scratch);

    let (code, _, err) = jrs(&scratch, &root, &["build"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        line_of(&err, "Compiling core v1.0.0") < line_of(&err, "Compiling app v2.0.0"),
        "core is compiled before the member that depends on it:\n{err}"
    );
    assert!(err.contains("3 classes in 3 modules"), "{err}");
    assert!(
        root.join("app/target/classes/demo/app/Main.class")
            .is_file()
    );
    assert!(
        !root.join("app/jrs.lock").exists(),
        "a member has no lockfile of its own"
    );

    let (code, out, err) = jrs(&scratch, &root, &["run", "--", "workspace"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "Hello, workspace");
    assert!(err.contains("Running demo.app.Main"), "{err}");
}

#[test]
fn a_fat_jar_carries_its_sibling_members_classes() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("ws-fat");
    let root = workspace(&scratch);

    let (code, _, err) = jrs(&scratch, &root, &["package", "--fat"]);
    assert_eq!(code, 0, "{err}");
    // The libraries get their plain jars beside the application's fat one.
    assert!(root.join("core/target/core-1.0.0.jar").is_file());
    assert!(root.join("tool/target/tool-1.0.0.jar").is_file());
    let jar = root.join("app/target/app-2.0.0.jar");
    let output = std::process::Command::new(&toolchain.java)
        .arg("-jar")
        .arg(&jar)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Hello, world"
    );
    let names = jar_entries(&jar);
    assert!(
        names.contains(&"demo/core/Greeter.class".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"core.properties".to_string()), "{names:?}");

    // Two packagings of the same sources are byte for byte the same.
    let first = std::fs::read(&jar).unwrap();
    let (code, _, err) = jrs(&scratch, &root, &["package", "--fat", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(first, std::fs::read(&jar).unwrap());
}

#[test]
fn a_body_change_upstream_leaves_the_dependent_fresh() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-avoid");
    let root = workspace(&scratch);
    let (code, _, err) = jrs(&scratch, &root, &["build", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");

    let greeter = root.join("core/src/main/java/demo/core/Greeter.java");
    let source = std::fs::read_to_string(&greeter).unwrap();
    bump_mtime();
    std::fs::write(&greeter, source.replace("Hello, ", "Hi, ")).unwrap();
    let (code, _, err) = jrs(&scratch, &root, &["build", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Compiling core v1.0.0"), "{err}");
    assert!(
        err.contains("Fresh app v2.0.0"),
        "a body change does not reach app's compiler:\n{err}"
    );

    bump_mtime();
    std::fs::write(
        &greeter,
        source.replace(
            "public class Greeter {",
            "public class Greeter {\n    public static int answer() { return 42; }",
        ),
    )
    .unwrap();
    let (code, _, err) = jrs(&scratch, &root, &["build", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("Compiling app v2.0.0"),
        "an API change recompiles the member that depends on it:\n{err}"
    );
}

#[test]
fn a_member_narrows_the_command_to_itself_and_what_it_depends_on() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-narrow");
    let root = workspace(&scratch);

    // From inside the member's directory.
    let (code, _, err) = jrs(&scratch, &root.join("app"), &["build"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Compiling core v1.0.0"), "{err}");
    assert!(err.contains("Compiling app v2.0.0"), "{err}");
    assert!(!err.contains("tool"), "tool is not app's: {err}");
    assert!(
        !root.join("app/jrs.lock").exists(),
        "a member has no lockfile"
    );

    // With -p, from the root.
    let (code, _, err) = jrs(&scratch, &root, &["-p", "tool", "build"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("tool v1.0.0"), "{err}");
    assert!(!err.contains("app v2.0.0"), "{err}");

    let (code, _, err) = jrs(&scratch, &root, &["-p", "nope", "build"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("there is no member `nope`"), "{err}");

    // `jrs classpath` acts on one member, and puts its upstream's classes
    // where their jar would be.
    let (code, _, err) = jrs(&scratch, &root, &["classpath"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("name it with `-p <member>`"), "{err}");
    let (code, out, err) = jrs(&scratch, &root, &["-p", "app", "classpath"]);
    assert_eq!(code, 0, "{err}");
    let entries: Vec<&str> = out
        .trim()
        .split(if cfg!(windows) { ';' } else { ':' })
        .collect();
    assert!(entries[0].ends_with("classes"), "{entries:?}");
    assert!(
        Path::new(entries[1]).ends_with(Path::new("core/target/classes")),
        "{entries:?}"
    );
}

#[test]
fn run_picks_the_only_member_with_a_main_class() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-run");
    let root = workspace(&scratch);
    scratch.write(
        "ws/tool/jrs.toml",
        "[project]\nname = \"tool\"\nversion = \"1.0.0\"\nmain-class = \"demo.tool.Tool\"\n",
    );
    let (code, _, err) = jrs(&scratch, &root, &["run"]);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("more than one member has a main class"),
        "{err}"
    );
    assert!(
        err.contains("`app`, `tool`") || err.contains("`tool`, `app`"),
        "{err}"
    );
}

#[test]
fn one_lockfile_pins_one_version_for_every_member() {
    let scratch = Scratch::new("ws-lock");
    let repo = FixtureRepo::new(&scratch);
    // `a` asks for lib 2.0.0 itself; `b` reaches lib 1.0.0 through core.
    scratch.write(
        "ws/jrs.toml",
        &format!(
            "[workspace]\nmembers = [\"a\", \"b\"]\n\n{}",
            repo.manifest_section()
        ),
    );
    scratch.write(
        "ws/a/jrs.toml",
        "[project]\nname = \"a\"\nversion = \"1\"\n[dependencies]\n\"org.example:lib\" = \"2.0.0\"\n",
    );
    scratch.write(
        "ws/b/jrs.toml",
        "[project]\nname = \"b\"\nversion = \"1\"\n[dependencies]\n\"org.example:core\" = \"1.0.0\"\n",
    );
    let root = scratch.join("ws");
    let (code, out, err) = jrs(&scratch, &root, &["-p", "b", "classpath"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("lib-2.0.0.jar"),
        "b gets the workspace's lib:\n{out}"
    );
    assert!(!out.contains("lib-1.0.0.jar"), "{out}");
    assert!(out.contains("core-1.0.0.jar"), "{out}");
    let (code, out, err) = jrs(&scratch, &root, &["-p", "a", "classpath"]);
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains("core-1.0.0.jar"), "a has no core: {out}");

    let lock = std::fs::read_to_string(root.join("jrs.lock")).unwrap();
    assert!(lock.contains("version = 3\n"), "{lock}");
    assert!(lock.contains("members = [\"a\", \"b\"]\n"), "{lock}");
    assert!(!root.join("a/jrs.lock").exists());

    // The second run reads it back, and `jrs verify` checks it.
    let (code, _, err) = jrs(&scratch, &root, &["-v", "-p", "a", "classpath"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("reusing"), "{err}");
    let (code, _, err) = jrs(&scratch, &root, &["verify"]);
    assert_eq!(code, 0, "{err}");

    // `jrs tree` shows each member's share.
    let (code, out, err) = jrs(&scratch, &root, &["tree"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("a v1"), "{out}");
    assert!(out.contains("b v1"), "{out}");
}

#[test]
fn what_cannot_be_a_workspace_is_refused_with_a_reason() {
    let scratch = Scratch::new("ws-errors");
    scratch.write("c/jrs.toml", "[workspace]\nmembers = [\"x\", \"y\"]\n");
    scratch.write(
        "c/x/jrs.toml",
        "[project]\nname = \"x\"\nversion = \"1\"\n[dependencies]\ny = { path = \"../y\" }\n",
    );
    scratch.write(
        "c/y/jrs.toml",
        "[project]\nname = \"y\"\nversion = \"1\"\n[dependencies]\nx = { path = \"../x\" }\n",
    );
    let (code, _, err) = jrs(&scratch, &scratch.join("c"), &["build"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("in a cycle: x → y → x"), "{err}");

    // A path dependency outside a workspace has nothing to build it from.
    scratch.write(
        "lone/jrs.toml",
        "[project]\nname = \"lone\"\nversion = \"1\"\n[dependencies]\ncore = { path = \"../core\" }\n",
    );
    let (code, _, err) = jrs(&scratch, &scratch.join("lone"), &["build"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("only a workspace can build"), "{err}");

    // -p outside a workspace.
    scratch.write(
        "plain/jrs.toml",
        "[project]\nname = \"plain\"\nversion = \"1\"\n",
    );
    let (code, _, err) = jrs(&scratch, &scratch.join("plain"), &["-p", "x", "tree"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("not in a workspace"), "{err}");
}

#[test]
fn the_metadata_lists_every_member_with_its_modules() {
    let scratch = Scratch::new("ws-metadata");
    let root = workspace(&scratch);
    let (code, out, err) = jrs(&scratch, &root, &["metadata", "--no-deps"]);
    assert_eq!(code, 0, "{err}");
    let model = jrs::json::Json::parse(&out).unwrap();
    let members = model.get("members").map(jrs::json::Json::items).unwrap();
    assert_eq!(members.len(), 3);
    let app = members
        .iter()
        .find(|m| {
            m.get("project")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                == Some("app")
        })
        .unwrap();
    let modules = app.get("modules").map(jrs::json::Json::items).unwrap();
    assert_eq!(
        modules[0].get("name").and_then(|n| n.as_str()),
        Some("core")
    );
    assert_eq!(
        modules[0].get("classpath").and_then(|n| n.as_str()),
        Some("compile")
    );
    assert!(
        app.get("project")
            .and_then(|p| p.get("lockfile"))
            .and_then(|l| l.as_str())
            .is_some_and(|l| !l.contains("app")),
        "a member's lockfile is the root's"
    );

    // From a member, its own model.
    let (code, out, err) = jrs(&scratch, &root, &["-p", "core", "metadata", "--no-deps"]);
    assert_eq!(code, 0, "{err}");
    let model = jrs::json::Json::parse(&out).unwrap();
    assert_eq!(
        model
            .get("project")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str()),
        Some("core")
    );
}

#[test]
fn add_edits_the_chosen_member() {
    let scratch = Scratch::new("ws-add");
    let repo = FixtureRepo::new(&scratch);
    let root = workspace(&scratch);
    let mut root_manifest = std::fs::read_to_string(root.join("jrs.toml")).unwrap();
    root_manifest.push('\n');
    root_manifest.push_str(&repo.manifest_section());
    std::fs::write(root.join("jrs.toml"), root_manifest).unwrap();

    let (code, _, err) = jrs(&scratch, &root, &["add", "org.example:lib:2.0.0"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("which member should `jrs add` edit?"), "{err}");

    let (code, _, err) = jrs(
        &scratch,
        &root,
        &["-p", "core", "add", "org.example:lib:2.0.0"],
    );
    assert_eq!(code, 0, "{err}");
    let core = std::fs::read_to_string(root.join("core/jrs.toml")).unwrap();
    assert!(core.contains("\"org.example:lib\" = \"2.0.0\""), "{core}");
    let lock = std::fs::read_to_string(root.join("jrs.lock")).unwrap();
    assert!(lock.contains("artifact = \"lib\""), "{lock}");

    let (code, _, err) = jrs(&scratch, &root.join("core"), &["remove", "org.example:lib"]);
    assert_eq!(code, 0, "{err}");
    let core = std::fs::read_to_string(root.join("core/jrs.toml")).unwrap();
    assert!(!core.contains("org.example:lib"), "{core}");
}

/// Sources are compared by content once indexed, but the staleness check
/// starts from modification times: make the next write a later one.
fn bump_mtime() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

fn jar_entries(jar: &Path) -> Vec<String> {
    let file = std::fs::File::open(jar).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect()
}

#[test]
fn a_task_can_depend_on_another_members_task() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-tasks");
    let root = workspace(&scratch);
    scratch.write(
        "ws/core/jrs.toml",
        "[project]\nname = \"core\"\nversion = \"1.0.0\"\n\n\
         [tasks.stamp]\nscript = \"Stamp.java\"\n",
    );
    scratch.write(
        "ws/core/Stamp.java",
        "class Stamp { public static void main(String[] a) throws Exception {\n\
         java.nio.file.Files.createDirectories(java.nio.file.Path.of(\"target\"));\n\
         java.nio.file.Files.writeString(java.nio.file.Path.of(\"target/stamp.txt\"), \"stamped\"); } }\n",
    );
    scratch.write(
        "ws/app/jrs.toml",
        "[project]\nname = \"app\"\nversion = \"2.0.0\"\nmain-class = \"demo.app.Main\"\n\n\
         [dependencies]\ncore = { path = \"../core\" }\n\n\
         [tasks.check]\nscript = \"Check.java\"\ndepends-on = [\"core:stamp\"]\n\n\
         [hooks]\npost-compile = [\"check\"]\n",
    );
    scratch.write(
        "ws/app/Check.java",
        "class Check { public static void main(String[] a) throws Exception {\n\
         System.err.println(\"saw \" + java.nio.file.Files.readString(java.nio.file.Path.of(\"../core/target/stamp.txt\"))); } }\n",
    );
    let (code, _, err) = jrs(&scratch, &root, &["-p", "app", "build"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        line_of(&err, "Task core: stamp") < line_of(&err, "Task app: check (post-compile)"),
        "{err}"
    );
    assert!(err.contains("saw stamped"), "{err}");
    assert_eq!(
        err.matches("core: stamp").count(),
        1,
        "once per command:\n{err}"
    );

    // A name that is not there is caught when the workspace loads.
    scratch.write(
        "ws/app/jrs.toml",
        "[project]\nname = \"app\"\nversion = \"2.0.0\"\n\n\
         [tasks.check]\nscript = \"Check.java\"\ndepends-on = [\"core:nope\"]\n",
    );
    let (code, _, err) = jrs(&scratch, &root, &["build"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("`core` has no task `nope`"), "{err}");
}

/// One member's exclusion is its own: `a` keeps `lib` out of `core`'s graph,
/// and `b`, which reaches `core` through `wrapper` and excludes nothing,
/// still gets it, though the workspace walks `core` once.
#[test]
fn one_members_exclusion_does_not_take_a_jar_from_another() {
    use jrs::resolve::coord::Coord;
    let scratch = Scratch::new("ws-exclusions");
    let repo = FixtureRepo::new(&scratch);
    let wrapper = Coord::new("org.example", "wrapper", "1.0.0");
    repo.publish_pom(
        &wrapper,
        "<project><groupId>org.example</groupId><artifactId>wrapper</artifactId>\
         <version>1.0.0</version><dependencies><dependency><groupId>org.example</groupId>\
         <artifactId>core</artifactId><version>1.0.0</version></dependency>\
         </dependencies></project>",
    );
    repo.publish_jar(&wrapper, b"jar for wrapper");
    scratch.write(
        "ws/jrs.toml",
        &format!(
            "[workspace]\nmembers = [\"a\", \"b\"]\n\n{}",
            repo.manifest_section()
        ),
    );
    scratch.write(
        "ws/a/jrs.toml",
        "[project]\nname = \"a\"\nversion = \"1\"\n[dependencies]\n\
         \"org.example:core\" = { version = \"1.0.0\", exclusions = [\"org.example:lib\"] }\n",
    );
    scratch.write(
        "ws/b/jrs.toml",
        "[project]\nname = \"b\"\nversion = \"1\"\n[dependencies]\n\
         \"org.example:wrapper\" = \"1.0.0\"\n",
    );
    let root = scratch.join("ws");
    for round in ["resolved", "from jrs.lock"] {
        let (code, out, err) = jrs(&scratch, &root, &["-p", "b", "classpath"]);
        assert_eq!(code, 0, "{round}: {err}");
        assert!(out.contains("core-1.0.0.jar"), "{round}: {out}");
        assert!(
            out.contains("lib-1.0.0.jar"),
            "{round}: b excludes nothing: {out}"
        );
        let (code, out, err) = jrs(&scratch, &root, &["-p", "a", "classpath"]);
        assert_eq!(code, 0, "{round}: {err}");
        assert!(out.contains("core-1.0.0.jar"), "{round}: {out}");
        assert!(!out.contains("lib-"), "{round}: a excludes lib: {out}");
    }
}
