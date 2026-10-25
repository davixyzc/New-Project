//! Command-line entry point: `fly script.fly [args...]` or `fly -e "code"`.
//!
//! This binary has no Rust `main`: the C entry point below starts the command line in `cli`. That
//! skips the standard runtime's setup, which fly does not need. The unit tests are built as a test
//! harness, which needs its own `main`, so the entry point is not exported in that build.

#![cfg_attr(not(test), no_main)]

mod alloc;
mod ast;
mod builtins;
mod bytecode;
mod cli;
mod collection;
mod compiler;
mod jit;
mod lexer;
mod parser;
mod scalar;
mod value;
mod vm;

#[global_allocator]
static GLOBAL: alloc::Pooled = alloc::Pooled;

/// The C entry point. Returns the process exit status. The test harness has its own `main`, so the
/// symbol is only exported in a normal build; this function stays in the test build as well, which
/// keeps the command line reachable there.
#[cfg_attr(not(test), no_mangle)]
pub extern "C" fn main(
    _argc: std::ffi::c_int,
    _argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    cli::main()
}
