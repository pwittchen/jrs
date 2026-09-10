//! `jrs` — argument parsing and exit codes; everything else is the library.

fn main() {
    std::process::exit(jrs::cli::main());
}
