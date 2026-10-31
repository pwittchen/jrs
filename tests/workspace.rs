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
        .map(|i| archive.by_index(i).unwrap().name().unwrap().to_string())
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

/// A workspace of three for members built at once: `a` and `b` depend on
/// nothing, `c` on both.
fn diamond(scratch: &Scratch, dir: &str) -> std::path::PathBuf {
    scratch.write(
        &format!("{dir}/jrs.toml"),
        "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n\n[java]\nsource = 17\n",
    );
    for name in ["a", "b"] {
        scratch.write(
            &format!("{dir}/{name}/jrs.toml"),
            &format!("[project]\nname = \"{name}\"\nversion = \"1.0.0\"\n"),
        );
        for class in 0..20 {
            scratch.write(
                &format!("{dir}/{name}/src/main/java/demo/{name}/C{class}.java"),
                &format!(
                    "package demo.{name};\n\npublic class C{class} {{\n    \
                     public static int value() {{ return {class}; }}\n}}\n"
                ),
            );
        }
    }
    scratch.write(
        &format!("{dir}/c/jrs.toml"),
        "[project]\nname = \"c\"\nversion = \"1.0.0\"\n\n[dependencies]\n\
         a = { path = \"../a\" }\nb = { path = \"../b\" }\n",
    );
    scratch.write(
        &format!("{dir}/c/src/main/java/demo/c/Sum.java"),
        "package demo.c;\n\npublic class Sum {\n    public static int sum() {\n        \
         return demo.a.C1.value() + demo.b.C2.value();\n    }\n}\n",
    );
    scratch.join(dir)
}

/// The transcript without the lines that carry a duration.
fn untimed(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| !l.contains("Finished") && !l.trim_start().starts_with("time"))
        .map(str::to_string)
        .collect()
}

/// Members built at once print what one built after the other prints, and
/// write the same jars (`specs/WORKSPACES.md` §6.2).
#[test]
fn members_built_at_once_print_and_write_what_one_at_a_time_does() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-parallel");
    let root = diamond(&scratch, "ws");
    let jars = |root: &Path| -> Vec<Vec<u8>> {
        ["a", "b", "c"]
            .iter()
            .map(|m| {
                std::fs::read(root.join(m).join("target").join(format!("{m}-1.0.0.jar"))).unwrap()
            })
            .collect()
    };

    let (code, _, at_once) = jrs(
        &scratch,
        &root,
        &["--jobs", "4", "package", "--no-build-cache"],
    );
    assert_eq!(code, 0, "{at_once}");
    let parallel = jars(&root);
    let (code, _, err) = jrs(&scratch, &root, &["clean"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, in_turn) = jrs(
        &scratch,
        &root,
        &["--jobs", "1", "package", "--no-build-cache"],
    );
    assert_eq!(code, 0, "{in_turn}");
    assert_eq!(
        untimed(&at_once),
        untimed(&in_turn),
        "--jobs 4:\n{at_once}\n--jobs 1:\n{in_turn}"
    );
    assert!(
        parallel == jars(&root),
        "the jars differ between --jobs 4 and --jobs 1"
    );

    // The member rows of `--timings`, for the members that built at once.
    let (code, _, err) = jrs(&scratch, &root, &["clean"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = jrs(
        &scratch,
        &root,
        &["--jobs", "4", "build", "--timings", "--no-build-cache"],
    );
    assert_eq!(code, 0, "{err}");
    for member in ["a", "b", "c"] {
        assert!(err.contains(&format!("member {member} ")), "{err}");
    }
}

/// A compile error in one member built at once reports that member, after
/// the lines of the members before it in build order.
#[test]
fn a_member_that_fails_at_once_with_others_is_reported_in_build_order() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-parallel-fail");
    let root = diamond(&scratch, "ws");
    scratch.write(
        "ws/b/src/main/java/demo/b/Broken.java",
        "package demo.b;\n\npublic class Broken { int x = \"no\"; }\n",
    );

    let (code, _, err) = jrs(
        &scratch,
        &root,
        &["--jobs", "4", "build", "--no-build-cache"],
    );
    assert_eq!(code, 1, "{err}");
    let a = line_of(&err, "Compiling a v1.0.0");
    let b = line_of(&err, "Compiling b v1.0.0");
    let broken = line_of(&err, "Broken.java");
    let failed = line_of(&err, "error: compilation failed");
    assert!(a < b && b < broken && broken < failed, "{err}");
    assert!(!err.contains("Compiling c v1.0.0"), "c needs b: {err}");
}

/// A workspace of three whose members `a` and `b` have tests, tested with
/// the fake console launcher, with `workspace.parallel-tests` on. With
/// `RENDEZVOUS` set to a directory, each test waits there for the other
/// member's: they pass only when both run at once.
fn tested_workspace(
    scratch: &Scratch,
    toolchain: &jrs::toolchain::Toolchain,
) -> std::path::PathBuf {
    let repo = FixtureRepo::new(scratch);
    repo.publish_fake_launcher(scratch, toolchain);
    scratch.write(
        "ws/jrs.toml",
        &format!(
            "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\nparallel-tests = true\n\n\
             [java]\nsource = 17\n\n{}",
            repo.manifest_section()
        ),
    );
    for (name, other) in [("a", "b"), ("b", "a"), ("c", "")] {
        let dependencies = if name == "c" {
            "[dependencies]\na = { path = \"../a\" }\n".to_string()
        } else {
            format!(
                "[dev-dependencies]\n\"org.junit.platform:junit-platform-console-standalone\" \
                 = \"{}\"\n",
                common::FAKE_LAUNCHER_6
            )
        };
        scratch.write(
            &format!("ws/{name}/jrs.toml"),
            &format!("[project]\nname = \"{name}\"\nversion = \"1.0.0\"\n\n{dependencies}"),
        );
        scratch.write(
            &format!("ws/{name}/src/main/java/demo/{name}/Lib.java"),
            &format!("package demo.{name};\n\npublic class Lib {{\n    public static int one() {{ return 1; }}\n}}\n"),
        );
        if other.is_empty() {
            continue;
        }
        scratch.write(
            &format!("ws/{name}/src/test/java/demo/{name}/LibTest.java"),
            &format!(
                "package demo.{name};\n\nimport org.junit.jupiter.api.Test;\n\n\
                 class LibTest {{\n    @Test\n    void meets() throws Exception {{\n        \
                 String dir = System.getenv(\"RENDEZVOUS\");\n        \
                 if (dir == null) return;\n        \
                 java.nio.file.Files.writeString(java.nio.file.Path.of(dir, \"{name}\"), \"\");\n        \
                 long deadline = System.currentTimeMillis() + 60_000;\n        \
                 while (!java.nio.file.Files.exists(java.nio.file.Path.of(dir, \"{other}\"))) {{\n            \
                 if (System.currentTimeMillis() > deadline) throw new AssertionError(\"alone\");\n            \
                 Thread.sleep(20);\n        }}\n    }}\n}}\n"
            ),
        );
    }
    scratch.join("ws")
}

/// `workspace.parallel-tests`: the members' test JVMs run at once, and the
/// transcript is still the one tests run member by member print.
#[test]
fn members_tested_at_once_print_what_one_at_a_time_does() {
    let toolchain = require_jdk!();
    let scratch = Scratch::new("ws-parallel-tests");
    let root = tested_workspace(&scratch, &toolchain);
    let manifest = root.join("jrs.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();

    std::fs::write(&manifest, text.replace("parallel-tests = true\n", "")).unwrap();
    let (code, _, err) = jrs(&scratch, &root, &["--jobs", "4", "test", "--all"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, in_turn) = jrs(&scratch, &root, &["--jobs", "4", "test", "--all"]);
    assert_eq!(code, 0, "{in_turn}");
    std::fs::write(&manifest, &text).unwrap();
    let (code, _, at_once) = jrs(&scratch, &root, &["--jobs", "4", "test", "--all"]);
    assert_eq!(code, 0, "{at_once}");
    assert_eq!(
        untimed(&at_once),
        untimed(&in_turn),
        "--jobs 4:\n{at_once}\n--jobs 1:\n{in_turn}"
    );
    assert!(at_once.contains("2 passed"), "{at_once}");

    let rendezvous = scratch.join("rendezvous");
    std::fs::create_dir_all(&rendezvous).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jrs"))
        .arg("--manifest-path")
        .arg(&root)
        .args([
            "--progress",
            "never",
            "--color",
            "never",
            "--charset",
            "ascii",
        ])
        .args(["--jobs", "4", "test", "--all"])
        .env("JRS_CACHE_DIR", scratch.join("jrs-cache"))
        .env("JRS_CONFIG", scratch.join("no-config.toml"))
        .env("RENDEZVOUS", &rendezvous)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "the tests ran one at a time:\n{err}"
    );
}

/// An API change upstream compiles the sources of a dependent that use the
/// class that changed, not the whole member; a removal compiles what reaches
/// it, and a member none of whose sources uses it is only re-indexed.
#[test]
fn an_upstream_api_change_compiles_only_what_uses_the_changed_class() {
    let _toolchain = require_jdk!();
    let scratch = Scratch::new("ws-tracked");
    let root = workspace(&scratch);
    scratch.write(
        "ws/core/src/main/java/demo/core/Unused.java",
        "package demo.core;\n\npublic class Unused {\n    public static int one() { return 1; }\n}\n",
    );
    scratch.write(
        "ws/app/src/main/java/demo/app/Other.java",
        "package demo.app;\n\nclass Other {\n    int two() { return 2; }\n}\n",
    );
    let (code, _, err) = jrs(&scratch, &root, &["build", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");

    let greeter = root.join("core/src/main/java/demo/core/Greeter.java");
    let source = std::fs::read_to_string(&greeter).unwrap();
    bump_mtime();
    std::fs::write(
        &greeter,
        source.replace(
            "public class Greeter {",
            "public class Greeter {\n    public static int answer() { return 42; }",
        ),
    )
    .unwrap();
    let (code, _, err) = jrs(&scratch, &root, &["-v", "build", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Compiling app v2.0.0"), "{err}");
    assert!(
        err.contains("main unit: compiled 1 of 2 source files"),
        "only Main uses Greeter:\n{err}"
    );

    // A class app never names changes: app is stale, compiles nothing, and
    // the next build finds it fresh.
    let unused = root.join("core/src/main/java/demo/core/Unused.java");
    bump_mtime();
    std::fs::write(
        &unused,
        "package demo.core;\n\npublic class Unused {\n    public static int one() { return 1; }\n    \
         public static int two() { return 2; }\n}\n",
    )
    .unwrap();
    let (code, _, err) = jrs(&scratch, &root, &["-v", "build", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("main unit: compiled 0 of 2 source files"),
        "nothing in app uses Unused:\n{err}"
    );
    let (code, _, err) = jrs(&scratch, &root, &["build", "--no-build-cache"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Fresh app v2.0.0"), "{err}");

    // Taking `greet` away breaks Main, which has to be compiled — and fail.
    bump_mtime();
    std::fs::write(
        &greeter,
        "package demo.core;\n\npublic class Greeter {\n    public static int answer() { return 42; }\n}\n",
    )
    .unwrap();
    let (code, _, err) = jrs(&scratch, &root, &["build", "--no-build-cache"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("Main.java"), "{err}");
}

#[test]
fn install_writes_each_member_with_siblings_as_dependencies() {
    if jrs::toolchain::Toolchain::discover().is_err() {
        eprintln!("SKIPPED install_writes_each_member: no usable JDK");
        return;
    }
    let scratch = Scratch::new("ws-install");
    let root = workspace(&scratch);
    scratch.write(
        "ws/jrs.toml",
        "[workspace]\nmembers = [\"app\", \"core\", \"tool\"]\ngroup = \"com.acme\"\n\n\
         [java]\nsource = 17\n",
    );
    let repo = scratch.join("m2");
    let repo_arg = repo.display().to_string();

    // `-p app` takes `core`, which it depends on, and leaves `tool` alone.
    let (code, _, err) = jrs(
        &scratch,
        &root,
        &[
            "-p",
            "app",
            "install",
            "--repo",
            &repo_arg,
            "--no-build-cache",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        line_of(&err, "Installed com.acme:core:") < line_of(&err, "Installed com.acme:app:"),
        "{err}"
    );
    assert!(!repo.join("com/acme/tool").exists(), "{err}");

    let pom = std::fs::read_to_string(repo.join("com/acme/app/2.0.0/app-2.0.0.pom")).unwrap();
    assert!(
        pom.contains("<artifactId>core</artifactId>")
            && pom.contains("<groupId>com.acme</groupId>\n      <artifactId>core"),
        "{pom}"
    );
    assert!(repo.join("com/acme/core/1.0.0/core-1.0.0.jar").is_file());
}

#[test]
fn a_member_group_beats_the_workspace_and_the_root_project_gives_one() {
    let scratch = Scratch::new("ws-group");
    let root = scratch.write(
        "ws/jrs.toml",
        "[workspace]\nmembers = [\"a\", \"b\"]\n\n\
         [project]\nname = \"root\"\nversion = \"1\"\ngroup = \"com.root\"\n",
    );
    scratch.write(
        "ws/a/jrs.toml",
        "[project]\nname = \"a\"\nversion = \"1\"\n",
    );
    scratch.write(
        "ws/b/jrs.toml",
        "[project]\nname = \"b\"\nversion = \"1\"\ngroup = \"com.own\"\n",
    );
    let text = std::fs::read_to_string(&root).unwrap();
    let table: toml::Table = text.parse().unwrap();
    let workspace = jrs::workspace::Workspace::load(&root, &table).unwrap();
    let group = |name: &str| {
        workspace
            .members
            .iter()
            .find(|m| m.manifest.name == name)
            .and_then(|m| m.manifest.group.clone())
    };
    assert_eq!(group("a").as_deref(), Some("com.root"));
    assert_eq!(group("b").as_deref(), Some("com.own"));
}

#[test]
fn a_workspace_group_must_be_a_group() {
    let scratch = Scratch::new("ws-bad-group");
    let root = workspace(&scratch);
    scratch.write(
        "ws/jrs.toml",
        "[workspace]\nmembers = [\"app\", \"core\", \"tool\"]\ngroup = \"../up\"\n",
    );
    let (code, _, err) = jrs(&scratch, &root, &["build"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("workspace.group"), "{err}");
}
