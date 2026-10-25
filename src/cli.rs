//! The command line: `fly script.fly [args...]` or `fly -e "code"`. It runs on a thread with a
//! large stack, except for help and version.

use std::ffi::c_int;
use std::io::Write;
use std::path::PathBuf;

use crate::vm::Vm;

const USAGE: &str = "usage: fly [options] [script.fly] [args...]

  -e, --eval <code>   run inline code instead of a file
  -v, --version       print the version
  -h, --help          print this help";

/// Stack for the interpreter thread. Native callbacks that re-enter JavaScript recurse on the
/// Rust stack, so this leaves plenty of room for `MAX_NESTED_CALLS` levels.
const STACK_BYTES: usize = 256 * 1024 * 1024;

/// Runs the command line and returns the exit status. The C entry point in `main.rs` calls this.
pub fn main() -> i32 {
    ignore_sigpipe();
    let status = run();
    // The run has flushed fly's own buffer. This covers a partial line left in stdout's buffer,
    // which the runtime would have flushed on its way out.
    let _ = std::io::stdout().flush();
    status
}

/// The standard runtime ignores SIGPIPE, so a closed pipe is a write error that fly ignores
/// rather than a signal that ends the process. Keep that behaviour.
#[cfg(unix)]
fn ignore_sigpipe() {
    extern "C" {
        fn signal(signum: c_int, handler: usize) -> usize;
    }
    const SIGPIPE: c_int = 13;
    const SIG_IGN: usize = 1;
    // SAFETY: `signal` only changes how this process handles SIGPIPE.
    unsafe {
        signal(SIGPIPE, SIG_IGN);
    }
}

#[cfg(not(unix))]
fn ignore_sigpipe() {}

fn run() -> i32 {
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
        Ok(handle) => handle.join().unwrap_or(1),
        Err(_) => 1,
    }
}

/// Ends a run without freeing the VM. Its output is already flushed, and the process is about to
/// exit, which returns the memory anyway. Freeing every global one by one costs more than a small
/// script does.
fn exit_without_freeing(machine: Vm) {
    std::mem::forget(machine);
}

fn run_cli(args: Vec<String>) -> i32 {
    match args.get(1).map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            println!("{}", USAGE);
            0
        }
        Some("-v") | Some("--version") => {
            println!("fly {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("-e") | Some("--eval") => {
            let Some(code) = args.get(2) else {
                eprintln!("fly: --eval needs a value");
                return 2;
            };
            let mut argv = vec!["fly".to_string()];
            argv.extend(args.iter().skip(3).cloned());
            let cwd = std::env::current_dir().unwrap_or_default();
            let mut machine = Vm::new(argv);
            let status = machine.run_source(&cwd.join("[eval]"), code);
            exit_without_freeing(machine);
            status
        }
        Some(file) => {
            let path = PathBuf::from(file);
            let abs = match std::fs::canonicalize(&path) {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("fly: cannot find '{}'", file);
                    return 1;
                }
            };
            let mut argv = vec!["fly".to_string(), abs.display().to_string()];
            argv.extend(args.iter().skip(2).cloned());
            let mut machine = Vm::new(argv);
            let status = machine.run_main(&abs);
            exit_without_freeing(machine);
            status
        }
    }
}
