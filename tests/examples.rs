//! Runs every `tests/fixtures/*.fly` script with the `fly` binary and compares stdout with
//! the matching `.out` file. A `.exit` file, when present, holds the expected exit code.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn fixtures_match_expected_output() {
    let dir = fixtures_dir();
    let mut scripts: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("fixtures directory exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "fly"))
        .collect();
    scripts.sort();
    assert!(
        !scripts.is_empty(),
        "no fixtures found in {}",
        dir.display()
    );

    let mut failures = Vec::new();
    for script in &scripts {
        let name = script.file_name().unwrap().to_string_lossy().into_owned();
        let expected = fs::read_to_string(script.with_extension("out"))
            .unwrap_or_else(|_| panic!("{} has no .out file", name));
        let expected_code: i32 = fs::read_to_string(script.with_extension("exit"))
            .map(|s| s.trim().parse().expect("exit file holds a number"))
            .unwrap_or(0);

        let output = Command::new(env!("CARGO_BIN_EXE_fly"))
            .arg(&name)
            .current_dir(&dir)
            .output()
            .expect("fly runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let code = output.status.code().unwrap_or(-1);
        if stdout != expected || code != expected_code {
            failures.push(format!(
                "{}: exit {} (expected {})\n--- stdout\n{}\n--- expected\n{}\n--- stderr\n{}",
                name,
                code,
                expected_code,
                stdout,
                expected,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} fixture(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn eval_flag_runs_inline_code() {
    let output = Command::new(env!("CARGO_BIN_EXE_fly"))
        .args(["-e", "console.log([1, 2, 3].map(x => x * x).join(' '))"])
        .output()
        .expect("fly runs");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "1 4 9\n");
}

#[test]
fn syntax_errors_exit_with_status_one() {
    let output = Command::new(env!("CARGO_BIN_EXE_fly"))
        .args(["-e", "let = ;"])
        .output()
        .expect("fly runs");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("SyntaxError"));
}
