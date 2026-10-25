//! Command-line entry point: `fly script.fly [args...]` or `fly -e "code"`.

mod ast;
mod builtins;
mod bytecode;
mod compiler;
mod jit;
mod lexer;
mod parser;
mod scalar;
mod value;
mod vm;

use std::path::PathBuf;
use std::process::ExitCode;

use vm::Vm;

const USAGE: &str = "usage: fly [options] [script.fly] [args...]

  -e, --eval <code>   run inline code instead of a file
  -v, --version       print the version
  -h, --help          print this help";

/// Stack for the interpreter thread. Native callbacks that re-enter JavaScript recurse on the
/// Rust stack, so this leaves plenty of room for `MAX_NESTED_CALLS` levels.
const STACK_BYTES: usize = 256 * 1024 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // Help and version need no interpreter, so they skip the thread that hosts it.
    if matches!(
        args.get(1).map(String::as_str),
        None | Some("-h" | "--help" | "-v" | "--version")
    ) {
        return run_cli(args);
    }
    let worker = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || run_cli(args));
    match worker {
        Ok(handle) => handle.join().unwrap_or(ExitCode::FAILURE),
        Err(_) => ExitCode::FAILURE,
    }
}

/// Ends a run without freeing the VM. Its output is already flushed, and the process is about to
/// exit, which returns the memory anyway. Freeing every global one by one costs more than a small
/// script does.
fn exit_without_freeing(machine: Vm) {
    std::mem::forget(machine);
}

fn run_cli(args: Vec<String>) -> ExitCode {
    match args.get(1).map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            println!("{}", USAGE);
            ExitCode::SUCCESS
        }
        Some("-v") | Some("--version") => {
            println!("fly {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("-e") | Some("--eval") => {
            let Some(code) = args.get(2) else {
                eprintln!("fly: --eval needs a value");
                return ExitCode::from(2);
            };
            let mut argv = vec!["fly".to_string()];
            argv.extend(args.iter().skip(3).cloned());
            let cwd = std::env::current_dir().unwrap_or_default();
            let mut machine = Vm::new(argv);
            let status = machine.run_source(&cwd.join("[eval]"), code);
            exit_without_freeing(machine);
            ExitCode::from(status as u8)
        }
        Some(file) => {
            let path = PathBuf::from(file);
            let abs = match std::fs::canonicalize(&path) {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("fly: cannot find '{}'", file);
                    return ExitCode::from(1);
                }
            };
            let mut argv = vec!["fly".to_string(), abs.display().to_string()];
            argv.extend(args.iter().skip(2).cloned());
            let mut machine = Vm::new(argv);
            let status = machine.run_main(&abs);
            exit_without_freeing(machine);
            ExitCode::from(status as u8)
        }
    }
}
