//! Shell completion scripts for bash, zsh and fish.
//!
//! The scripts are generated from the same `clap` definition the parser uses,
//! so a new flag or subcommand is completable the moment it is parseable —
//! nobody has to remember to edit a script. [`generate`] walks the built
//! command tree once into a small [`Node`] model, and one writer per shell
//! turns that model into text.
//!
//! Hand-rolled rather than `clap_complete`, for the reason the crate list in
//! SPEC §13.1 is short: every dependency costs compile time and readability,
//! and this is a few hundred lines of string building. It is the trade §13.10
//! made for the progress renderer — code a reader can follow end to end over
//! code they have to trust. The price is scope, and it is paid deliberately:
//!
//! * subcommands (recursively, so `jrs cache prune` needs no special case),
//!   with their `about` line as the description where the shell shows one;
//! * every visible flag, long and short, including `global = true` ones, which
//!   `clap` copies into each subcommand when the command is built;
//! * values from a fixed set (`--progress auto|always|never`), and file names
//!   for options that take a path and for positional arguments;
//! * nothing else. A free-form value (`--jobs N`) completes to nothing rather
//!   than to a guess, and nothing calls back into jrs at completion time.
//!
//! Hidden arguments, subcommands and possible values are skipped throughout.

use clap::{Arg, Command, ValueHint};

/// A shell `generate` can write a script for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub const ALL: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];

    /// The shell named `s`, as spelled by [`Shell::as_str`], ignoring case.
    pub fn parse(s: &str) -> Option<Shell> {
        Shell::ALL
            .into_iter()
            .find(|shell| shell.as_str().eq_ignore_ascii_case(s))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
}

/// The complete completion script for `cmd` in `shell`.
pub fn generate(shell: Shell, cmd: &Command) -> String {
    // `build` is what adds `--help`/`--version` and copies global args down
    // into every subcommand; before it, a subcommand only knows its own args.
    let mut cmd = cmd.clone();
    cmd.build();
    let root = Node::new(&cmd, &[]);
    let mut script = match shell {
        Shell::Bash => bash(&root),
        Shell::Zsh => zsh(&root),
        Shell::Fish => fish(&root),
    }
    .join("\n");
    script.push('\n');
    script
}

/// One (sub)command, reduced to what completion needs.
struct Node {
    /// Command names from the binary down: `["jrs", "cache", "prune"]`.
    path: Vec<String>,
    about: String,
    opts: Vec<Opt>,
    /// Takes positional arguments. They are offered file names, as the only
    /// guess that is right more often than it is noise.
    positional: bool,
    subs: Vec<Node>,
}

struct Opt {
    short: Option<char>,
    long: Option<String>,
    help: String,
    value_name: String,
    value: Value,
    /// Declared `global = true`: valid, meaning the same thing, at any level.
    global: bool,
}

/// What follows an option on the command line.
enum Value {
    None,
    Choices(Vec<String>),
    Path,
    Free,
}

impl Node {
    fn new(cmd: &Command, parent: &[String]) -> Node {
        let mut path = parent.to_vec();
        path.push(cmd.get_name().to_string());
        Node {
            about: one_line(cmd.get_about()),
            opts: (cmd.get_arguments())
                .filter(|arg| !arg.is_hide_set() && !arg.is_positional())
                .map(Opt::new)
                .collect(),
            positional: cmd.get_positionals().any(|arg| !arg.is_hide_set()),
            subs: (cmd.get_subcommands())
                .filter(|sub| !sub.is_hide_set())
                .map(|sub| Node::new(sub, &path))
                .collect(),
            path,
        }
    }

    fn name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    /// `jrs__cache__prune`: unique per node, and safe as a shell identifier.
    fn id(&self) -> String {
        self.path.join("__")
    }

    /// This node and every node below it, parents first.
    fn walk(&self) -> Vec<&Node> {
        let mut nodes = vec![self];
        for sub in &self.subs {
            nodes.extend(sub.walk());
        }
        nodes
    }
}

impl Opt {
    fn new(arg: &Arg) -> Opt {
        let value = if !arg.get_action().takes_values() {
            Value::None
        } else {
            let choices: Vec<String> = (arg.get_possible_values().iter())
                .filter(|value| !value.is_hide_set())
                .map(|value| value.get_name().to_string())
                .collect();
            if !choices.is_empty() {
                Value::Choices(choices)
            } else if takes_path(arg) {
                Value::Path
            } else {
                Value::Free
            }
        };
        Opt {
            short: arg.get_short(),
            long: arg.get_long().map(str::to_string),
            help: one_line(arg.get_help()),
            value_name: (arg.get_value_names().and_then(|names| names.first())).map_or_else(
                || arg.get_id().to_string().to_uppercase(),
                |n| n.to_string(),
            ),
            value,
            global: arg.is_global_set(),
        }
    }

    /// `-j`, `--jobs`: every way to spell this option.
    fn spellings(&self) -> Vec<String> {
        let short = self.short.map(|c| format!("-{c}"));
        let long = self.long.as_ref().map(|l| format!("--{l}"));
        short.into_iter().chain(long).collect()
    }
}

/// A `PathBuf` argument carries a path hint by itself; a `String` one named
/// `PATH` or `DIR` is taken at its word.
fn takes_path(arg: &Arg) -> bool {
    let hinted = matches!(
        arg.get_value_hint(),
        ValueHint::AnyPath | ValueHint::FilePath | ValueHint::DirPath | ValueHint::ExecutablePath
    );
    let named = (arg.get_value_names().unwrap_or_default().iter()).any(|name| {
        let name = name.to_uppercase();
        ["PATH", "DIR", "FILE"].iter().any(|k| name.contains(k))
    });
    hinted || named
}

/// Help text on one line: every shell here shows a description as one row.
fn one_line(text: Option<&clap::builder::StyledStr>) -> String {
    let text = text.map(ToString::to_string).unwrap_or_default();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Quoting. Every piece of help text and every name is quoted for the shell it
// lands in, including the inner layer where a word list is itself re-parsed
// (bash's `compgen -W`, fish's `-a`), so a stray `'` or `$` stays inert.

/// Words that need no quoting in any of the three shells.
fn is_plain(word: &str) -> bool {
    !word.is_empty() && (word.chars()).all(|c| c.is_ascii_alphanumeric() || "-_.,+/".contains(c))
}

/// A POSIX single-quoted string (bash and zsh): only `'` needs care.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A fish single-quoted string, where `\\` and `\'` are escapes.
fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"))
}

/// A space-separated list of words, each quoted by `quote` if it needs it.
fn word_list<'a>(words: impl IntoIterator<Item = &'a str>, quote: fn(&str) -> String) -> String {
    let words = words.into_iter();
    let quoted: Vec<String> = words
        .map(|w| if is_plain(w) { w.to_string() } else { quote(w) })
        .collect();
    quoted.join(" ")
}

/// Inside an `_arguments` spec, `[`, `]` and `:` are syntax; `\` makes them text.
fn zsh_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '[' | ']' | ':') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A value inside an `_arguments` `(a b c)` action: escape anything unusual.
fn zsh_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !(c.is_ascii_alphanumeric() || "-_.,+/".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A `case` pattern matching `"$cmd,$word"` for this option in `node`. Global
/// options match under any command, since `clap` accepts them at every level.
fn bash_patterns(node: &Node, opt: &Opt) -> String {
    let patterns: Vec<String> = (opt.spellings().iter())
        .map(|flag| match opt.global {
            true => format!("*{}", sh_quote(&format!(",{flag}"))),
            false => sh_quote(&format!("{},{flag}", node.id())),
        })
        .collect();
    patterns.join("|")
}

fn push_unique(lines: &mut Vec<String>, line: String) {
    if !lines.contains(&line) {
        lines.push(line);
    }
}

/// Bash: one function that scans `COMP_WORDS` to find the active command.
///
/// Written for bash 3.2, which macOS still ships: no associative arrays, no
/// `mapfile`, and `compopt` only where it exists.
fn bash(root: &Node) -> Vec<String> {
    let bin = root.name();
    let func = format!("_{bin}");
    let (mut scan, mut skip, mut values, mut words) = (vec![], vec![], vec![], vec![]);
    for node in root.walk() {
        for sub in &node.subs {
            let pattern = sh_quote(&format!("{},{}", node.id(), sub.name()));
            scan.push(format!(
                "            {pattern}) cmd={} ;;",
                sh_quote(&sub.id())
            ));
        }
        for opt in &node.opts {
            let action = match &opt.value {
                Value::None => continue,
                Value::Choices(choices) => {
                    let list = word_list(choices.iter().map(String::as_str), sh_quote);
                    format!("COMPREPLY=($(compgen -W {} -- \"$cur\"))", sh_quote(&list))
                }
                Value::Path => format!("{func}_files \"$cur\""),
                // A value is due, but there is nothing worth offering.
                Value::Free => ":".to_string(),
            };
            let patterns = bash_patterns(node, opt);
            push_unique(
                &mut skip,
                format!("            {patterns}) i=$((i + 1)) ;;"),
            );
            push_unique(
                &mut values,
                format!("        {patterns}) {action}; return 0 ;;"),
            );
        }
        let subs = word_list(node.subs.iter().map(Node::name), sh_quote);
        let flags: Vec<String> = node.opts.iter().flat_map(Opt::spellings).collect();
        let flags = word_list(flags.iter().map(String::as_str), sh_quote);
        words.push(format!(
            "        {}) subs={}; flags={}; files={} ;;",
            sh_quote(&node.id()),
            sh_quote(&subs),
            sh_quote(&flags),
            u8::from(node.positional),
        ));
    }

    let mut out = vec![
        format!("# bash completion for {bin}. Generated by `{bin} completions bash`."),
        format!("# Load with `source <({bin} completions bash)`."),
        String::new(),
        format!("{func}_files() {{"),
        "    local IFS=$'\\n'".into(),
        "    # bash 3.2 has no compopt; there, directories just lack a trailing slash.".into(),
        "    type compopt >/dev/null 2>&1 && compopt -o filenames 2>/dev/null".into(),
        "    COMPREPLY=($(compgen -f -- \"$1\"))".into(),
        "}".into(),
        String::new(),
        format!("{func}() {{"),
        "    local cur prev w i=1 cmd=".to_string() + &sh_quote(&root.id()),
        "    local dashdash='' subs='' flags='' files=0".into(),
        "    COMPREPLY=()".into(),
        "    cur=\"${COMP_WORDS[COMP_CWORD]}\"".into(),
        "    prev=\"${COMP_WORDS[COMP_CWORD-1]}\"".into(),
        "    # COMP_WORDS splits `--opt=value` into `--opt`, `=`, `value`.".into(),
        "    if [ \"$cur\" = \"=\" ]; then".into(),
        "        cur=''".into(),
        "    elif [ \"$prev\" = \"=\" ]; then".into(),
        "        prev=\"${COMP_WORDS[COMP_CWORD-2]}\"".into(),
        "    fi".into(),
        String::new(),
        "    # Find the active (sub)command, stepping over option values so that".into(),
        "    # `--manifest-path build` is not mistaken for the `build` command.".into(),
        "    while [ \"$i\" -lt \"$COMP_CWORD\" ]; do".into(),
        "        w=\"${COMP_WORDS[i]}\"".into(),
        "        if [ \"${COMP_WORDS[i+1]}\" = \"=\" ]; then".into(),
        "            i=$((i + 3))".into(),
        "            continue".into(),
        "        fi".into(),
        "        case \"$cmd,$w\" in".into(),
        "            *,--) dashdash=1; break ;;".into(),
    ];
    out.extend(scan);
    out.extend(skip);
    out.extend([
        "        esac".into(),
        "        i=$((i + 1))".into(),
        "    done".into(),
        "    if [ -n \"$dashdash\" ]; then".into(),
        format!("        {func}_files \"$cur\""),
        "        return 0".into(),
        "    fi".into(),
        String::new(),
        "    case \"$cmd,$prev\" in".into(),
    ]);
    out.extend(values);
    out.extend([
        "    esac".into(),
        String::new(),
        "    case \"$cmd\" in".into(),
    ]);
    out.extend(words);
    out.extend([
        "    esac".into(),
        "    case \"$cur\" in".into(),
        "        -*) COMPREPLY=($(compgen -W \"$flags\" -- \"$cur\")) ;;".into(),
        "        *)".into(),
        "            if [ -n \"$subs\" ]; then".into(),
        "                COMPREPLY=($(compgen -W \"$subs\" -- \"$cur\"))".into(),
        "            elif [ \"$files\" = 1 ]; then".into(),
        format!("                {func}_files \"$cur\""),
        "            else".into(),
        "                COMPREPLY=($(compgen -W \"$flags\" -- \"$cur\"))".into(),
        "            fi".into(),
        "            ;;".into(),
        "    esac".into(),
        "    return 0".into(),
        "}".into(),
        String::new(),
        format!("complete -F {func} {bin}"),
    ]);
    out
}

/// `head`, then each item on its own backslash-continued line.
fn continued(head: &str, items: &[String]) -> String {
    let mut out = head.to_string();
    for item in items {
        out.push_str(" \\\n        ");
        out.push_str(item);
    }
    out
}

/// Zsh `_arguments` specs for one option, one per spelling.
fn zsh_specs(opt: &Opt) -> Vec<String> {
    let help = zsh_escape(&opt.help);
    let message = zsh_escape(&opt.value_name);
    let tail = match &opt.value {
        Value::None => String::new(),
        Value::Choices(choices) => {
            let values: Vec<String> = choices.iter().map(|v| zsh_value(v)).collect();
            format!(":{message}:({})", values.join(" "))
        }
        Value::Path => format!(":{message}:_files"),
        Value::Free => format!(":{message}: "),
    };
    // `-j+` and `--jobs=` accept the value attached or as the next word.
    let short = opt.short.map(|c| (format!("-{c}"), "+"));
    let long = opt.long.as_ref().map(|l| (format!("--{l}"), "="));
    (short.into_iter().chain(long))
        .map(|(flag, glue)| {
            let glue = if matches!(opt.value, Value::None) {
                ""
            } else {
                glue
            };
            sh_quote(&format!("{flag}{glue}[{help}]{tail}"))
        })
        .collect()
}

/// Zsh: one function per command; a command with subcommands hands the rest
/// of the line to the chosen subcommand's function via `*::`, which narrows
/// `$words` so `$words[1]` is the subcommand's name.
fn zsh(root: &Node) -> Vec<String> {
    let bin = root.name();
    let mut out = vec![
        format!("#compdef {bin}"),
        format!("# zsh completion for {bin}. Generated by `{bin} completions zsh`."),
        format!("# Load with `source <({bin} completions zsh)`, or save as _{bin} on $fpath."),
    ];
    for node in root.walk() {
        let mut specs: Vec<String> = node.opts.iter().flat_map(zsh_specs).collect();
        out.push(String::new());
        out.push(format!("_{}() {{", node.id()));
        if node.subs.is_empty() {
            if node.positional {
                specs.push("'*::file:_files'".into());
            }
            out.push(continued("    _arguments -s -S", &specs));
            out.push("}".into());
            continue;
        }
        specs.extend(["'1: :->command'".into(), "'*:: :->args'".into()]);
        out.push("    local curcontext=\"$curcontext\" state line ret=1".into());
        out.push(continued("    _arguments -s -S -C", &specs) + " \\\n        && ret=0");
        out.extend([
            "    case $state in".into(),
            "        (command)".into(),
            "            local -a commands".into(),
            "            commands=(".into(),
        ]);
        for sub in &node.subs {
            let entry = format!("{}:{}", sub.name().replace(':', "\\:"), sub.about);
            out.push(format!("                {}", sh_quote(&entry)));
        }
        out.extend([
            "            )".into(),
            format!(
                "            _describe -t commands {} commands && ret=0",
                sh_quote(&format!("{} command", node.path.join(" ")))
            ),
            "            ;;".into(),
            "        (args)".into(),
            format!(
                "            curcontext=\"${{curcontext%:*:*}}:{}-$words[1]:\"",
                node.path.join("-")
            ),
            "            case $words[1] in".into(),
        ]);
        for sub in &node.subs {
            let arm = sh_quote(sub.name());
            out.push(format!("                ({arm}) _{} && ret=0 ;;", sub.id()));
        }
        out.extend([
            "            esac".into(),
            "            ;;".into(),
            "    esac".into(),
            "    return ret".into(),
            "}".into(),
        ]);
    }
    // Autoloaded from $fpath the file *is* `_jrs`, so run it; sourced, register it.
    out.extend([
        String::new(),
        format!("if [ \"$funcstack[1]\" = \"_{bin}\" ]; then"),
        format!("    _{bin} \"$@\""),
        "else".into(),
        format!("    compdef _{bin} {bin}"),
        "fi".into(),
    ]);
    out
}

/// The fish condition under which `node` is the active command. Nested names
/// are matched anywhere on the line — fish has no cheaper positional check —
/// and a node with subcommands stops applying once one of them is typed.
fn fish_condition(node: &Node) -> String {
    let names = &node.path[1..];
    if names.is_empty() {
        return "__fish_use_subcommand".into();
    }
    let mut parts: Vec<String> = (names.iter())
        .map(|name| {
            format!(
                "__fish_seen_subcommand_from {}",
                word_list([name.as_str()], fish_quote)
            )
        })
        .collect();
    if !node.subs.is_empty() {
        let subs = word_list(node.subs.iter().map(Node::name), fish_quote);
        parts.push(format!("not __fish_seen_subcommand_from {subs}"));
    }
    parts.join("; and ")
}

/// Fish: one `complete` line per option and per subcommand.
fn fish(root: &Node) -> Vec<String> {
    let bin = root.name();
    let mut out = vec![
        format!("# fish completion for {bin}. Generated by `{bin} completions fish`."),
        format!("# Load with `{bin} completions fish | source`."),
        String::new(),
        "# No file names unless a line below asks for them.".into(),
        format!("complete -c {bin} -f"),
    ];
    for node in root.walk() {
        let head = format!("complete -c {bin} -n {}", fish_quote(&fish_condition(node)));
        for opt in &node.opts {
            let mut line = head.clone();
            if let Some(c) = opt.short {
                line.push_str(&format!(
                    " -s {}",
                    word_list([c.to_string().as_str()], fish_quote)
                ));
            }
            if let Some(long) = &opt.long {
                line.push_str(&format!(" -l {}", word_list([long.as_str()], fish_quote)));
            }
            match &opt.value {
                Value::None => {}
                Value::Choices(choices) => {
                    let list = word_list(choices.iter().map(String::as_str), fish_quote);
                    line.push_str(&format!(" -x -a {}", fish_quote(&list)));
                }
                Value::Path => line.push_str(" -r -F"),
                Value::Free => line.push_str(" -x"),
            }
            if !opt.help.is_empty() {
                line.push_str(&format!(" -d {}", fish_quote(&opt.help)));
            }
            out.push(line);
        }
        for sub in &node.subs {
            let mut line = format!("{head} -a {}", fish_quote(sub.name()));
            if !sub.about.is_empty() {
                line.push_str(&format!(" -d {}", fish_quote(&sub.about)));
            }
            out.push(line);
        }
        if node.positional && node.subs.is_empty() {
            out.push(format!("{head} -F"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::CommandFactory;
    use clap::builder::PossibleValue;
    use std::path::PathBuf;

    fn jrs(shell: Shell) -> String {
        generate(shell, &Cli::command())
    }

    /// A made-up CLI with the awkward cases the real one does not have yet:
    /// nesting, hidden items, a quote and brackets in help text.
    fn tool() -> Command {
        Command::new("tool")
            .arg(Arg::new("secret").long("secret").hide(true).num_args(0))
            .arg(
                Arg::new("mode")
                    .long("mode")
                    .help("It's [odd]: $HOME `x`")
                    .value_parser([
                        PossibleValue::new("fast"),
                        PossibleValue::new("stealth").hide(true),
                    ]),
            )
            .arg(Arg::new("out").short('o').long("out").value_name("FILE"))
            .subcommand(
                Command::new("cache")
                    .about("Manage the cache; it's shared")
                    .subcommand(
                        Command::new("prune")
                            .about("Drop old entries")
                            .arg(Arg::new("older").long("older-than").value_name("DAYS")),
                    )
                    .subcommand(Command::new("ghost").hide(true)),
            )
            .subcommand(Command::new("internal").hide(true))
    }

    #[test]
    fn shells_round_trip_through_their_names() {
        for shell in Shell::ALL {
            assert_eq!(Shell::parse(shell.as_str()), Some(shell));
        }
        assert_eq!(Shell::parse("ZSH"), Some(Shell::Zsh));
        assert_eq!(Shell::parse("tcsh"), None);
    }

    #[test]
    fn every_subcommand_is_offered_in_every_shell() {
        let cli = Cli::command();
        for shell in Shell::ALL {
            let script = jrs(shell);
            for sub in cli.get_subcommands() {
                assert!(
                    script.contains(sub.get_name()),
                    "{shell:?} lacks {}",
                    sub.get_name()
                );
            }
        }
        assert!(jrs(Shell::Bash).contains("subs='build test run package doc clean tree"));
        // Nested subcommands are offered under their parent.
        assert!(jrs(Shell::Bash).contains("subs='path prune"));
        assert!(jrs(Shell::Fish).contains("__fish_seen_subcommand_from cache"));
        assert!(jrs(Shell::Zsh).contains("'build:Resolve dependencies"));
        assert!(jrs(Shell::Fish).contains("-n '__fish_use_subcommand' -a 'build' -d 'Resolve"));
    }

    #[test]
    fn progress_values_are_offered_in_every_shell() {
        assert!(
            jrs(Shell::Bash)
                .contains("*',--progress') COMPREPLY=($(compgen -W 'auto always never'")
        );
        // clap drops the trailing period of a one-sentence doc comment.
        assert!(
            jrs(Shell::Zsh)
                .contains("'--progress=[Live animated output]:WHEN:(auto always never)'")
        );
        assert!(jrs(Shell::Fish).contains("-l progress -x -a 'auto always never'"));
    }

    #[test]
    fn subcommand_flags_are_offered() {
        assert!(jrs(Shell::Bash).contains("'jrs__migrate') subs=''; flags='--from --dry-run"));
        assert!(jrs(Shell::Zsh).contains("_jrs__migrate() {"));
        assert!(jrs(Shell::Zsh).contains("'--dry-run[Print the manifest"));
        assert!(jrs(Shell::Fish).contains("'__fish_seen_subcommand_from migrate' -l dry-run"));
    }

    #[test]
    fn path_values_complete_files() {
        assert!(jrs(Shell::Bash).contains("*',--manifest-path') _jrs_files \"$cur\"; return 0"));
        assert!(jrs(Shell::Zsh).contains("'--manifest-path=[Run against a manifest outside"));
        assert!(jrs(Shell::Zsh).contains("]:PATH:_files'"));
        assert!(jrs(Shell::Fish).contains("-l manifest-path -r -F"));
        // `--from SYSTEM` is free-form: a value is due, but none is invented.
        assert!(jrs(Shell::Bash).contains("'jrs__migrate,--from') :; return 0"));
        assert!(jrs(Shell::Fish).contains("-l from -x -d"));
    }

    #[test]
    fn hidden_items_are_skipped_and_nesting_recurses() {
        for shell in Shell::ALL {
            let script = generate(shell, &tool());
            for hidden in ["secret", "ghost", "internal", "stealth"] {
                assert!(!script.contains(hidden), "{shell:?} offers hidden {hidden}");
            }
            assert!(
                script.contains("prune") && script.contains("older-than"),
                "{shell:?}"
            );
        }
        let bash = generate(Shell::Bash, &tool());
        assert!(bash.contains("'tool__cache,prune') cmd='tool__cache__prune' ;;"));
        assert!(bash.contains("'tool,--out'|'tool,-o'") || bash.contains("'tool,-o'|'tool,--out'"));
        let zsh = generate(Shell::Zsh, &tool());
        assert!(zsh.contains("_tool__cache__prune() {"));
        assert!(zsh.contains("('prune') _tool__cache__prune && ret=0 ;;"));
        let fish = generate(Shell::Fish, &tool());
        assert!(fish.contains(
            "-n '__fish_seen_subcommand_from cache; and not __fish_seen_subcommand_from prune help'"
        ));
        assert!(fish.contains(
            "-n '__fish_seen_subcommand_from cache; and __fish_seen_subcommand_from prune' -l older-than"
        ));
    }

    #[test]
    fn help_text_is_escaped_per_shell() {
        let zsh = generate(Shell::Zsh, &tool());
        assert!(zsh.contains(r"'--mode=[It'\''s \[odd\]\: $HOME `x`]:MODE:(fast)'"));
        assert!(zsh.contains(r"'cache:Manage the cache; it'\''s shared'"));
        let fish = generate(Shell::Fish, &tool());
        assert!(fish.contains(r"-l mode -x -a 'fast' -d 'It\'s [odd]: $HOME `x`'"));
    }

    fn have(shell: &str) -> bool {
        let probe = std::process::Command::new(shell).arg("--version").output();
        if probe.is_err() {
            eprintln!("SKIPPED {}: no {shell} on PATH", module_path!());
        }
        probe.is_ok()
    }

    fn scratch(name: &str, text: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("jrs-completions-{}-{name}", std::process::id()));
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn scripts_pass_each_shells_syntax_check() {
        let checks = [
            (Shell::Bash, "-n"),
            (Shell::Zsh, "-n"),
            (Shell::Fish, "--no-execute"),
        ];
        for (shell, flag) in checks {
            if !have(shell.as_str()) {
                continue;
            }
            for (name, cmd) in [("jrs", Cli::command()), ("tool", tool())] {
                let path = scratch(
                    &format!("{name}.{}", shell.as_str()),
                    &generate(shell, &cmd),
                );
                let status = std::process::Command::new(shell.as_str())
                    .arg(flag)
                    .arg(&path)
                    .output()
                    .unwrap();
                std::fs::remove_file(&path).unwrap();
                assert!(
                    status.status.success(),
                    "{shell:?} rejects the {name} script: {status:?}"
                );
            }
        }
    }

    /// Run `_jrs` in a real bash with the given words, the last being the one
    /// under the cursor, and return what it offers.
    fn bash_complete(script: &std::path::Path, words: &[&str]) -> String {
        let driver = r#"source "$1"; shift; COMP_WORDS=("$@"); COMP_CWORD=$(($# - 1)); _jrs; echo "${COMPREPLY[*]}""#;
        let out = std::process::Command::new("bash")
            .args(["-c", driver, "bash"])
            .arg(script)
            .args(words)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn bash_completes_subcommands_and_option_values() {
        if !have("bash") {
            return;
        }
        let path = scratch("drive.bash", &jrs(Shell::Bash));
        assert_eq!(bash_complete(&path, &["jrs", "bu"]), "build");
        assert_eq!(
            bash_complete(&path, &["jrs", "--progress", ""]),
            "auto always never"
        );
        assert_eq!(
            bash_complete(&path, &["jrs", "--progress", "=", "a"]),
            "auto always"
        );
        assert_eq!(
            bash_complete(&path, &["jrs", "build", "--color", "n"]),
            "never"
        );
        assert_eq!(
            bash_complete(&path, &["jrs", "--jobs", "4", "mi"]),
            "migrate"
        );
        assert_eq!(
            bash_complete(&path, &["jrs", "--manifest-path", "build", "pa"]),
            "package"
        );
        assert!(bash_complete(&path, &["jrs", "migrate", "--d"]).contains("--dry-run"));
        assert_eq!(bash_complete(&path, &["jrs", "--jobs", ""]), "");
        std::fs::remove_file(&path).unwrap();
    }

    /// `source <(jrs completions zsh)` must register `_jrs`, not run it.
    #[test]
    fn zsh_registers_the_function_when_sourced() {
        if !have("zsh") {
            return;
        }
        let path = scratch("source.zsh", &jrs(Shell::Zsh));
        let driver =
            r#"autoload -U compinit; compinit -D -u; source "$1"; print -r -- "${_comps[jrs]}""#;
        let out = std::process::Command::new("zsh")
            .args(["-f", "-c", driver, "zsh"])
            .arg(&path)
            .output()
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "_jrs",
            "{out:?}"
        );
    }
}
