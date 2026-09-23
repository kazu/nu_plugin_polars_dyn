//! The argument handling of `nu-polars-dyn-build`: `--help` works, and each bad invocation
//! fails before anything is built, naming what is wrong.

use std::process::{Command, Output};

const BUILD: &str = env!("CARGO_BIN_EXE_nu-polars-dyn-build");

fn run(args: &[&str]) -> Output {
    Command::new(BUILD)
        .args(args)
        .output()
        .expect("nu-polars-dyn-build must run")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn fails_mentioning(args: &[&str], expected: &str) {
    let output = run(args);
    assert!(
        !output.status.success(),
        "{args:?} should fail\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = stderr(&output);
    assert!(
        stderr.contains(expected),
        "{args:?} should complain about `{expected}`\nstderr:\n{stderr}"
    );
}

#[test]
fn help_describes_every_argument() {
    for flag in ["--help", "-h"] {
        let output = run(&[flag]);
        assert!(output.status.success(), "{flag} should exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        for arg in [
            "<crate>...",
            "--path <name=dir>",
            "--git <name=url>",
            "--out <dir>",
            "--debug",
        ] {
            assert!(stdout.contains(arg), "{flag} should list `{arg}`\n{stdout}");
        }
    }
}

#[test]
fn a_missing_crate_fails() {
    fails_mentioning(&[], "<crate>...");
}

#[test]
fn an_unknown_flag_fails() {
    fails_mentioning(&["a", "--bogus"], "--bogus");
}

#[test]
fn a_path_without_an_equals_sign_fails() {
    fails_mentioning(&["a", "--path", "a"], "--path takes <name>=<value>");
}

#[test]
fn an_empty_value_fails() {
    fails_mentioning(&["a", "--git", "a="], "--git a= has no value");
}

#[test]
fn a_name_that_is_not_a_crate_fails() {
    fails_mentioning(
        &["a", "--path", "b=."],
        "`b`, which is not one of the crates",
    );
}

#[test]
fn a_name_given_to_both_path_and_git_fails() {
    fails_mentioning(
        &["a", "--git", "a=https://example.com/a", "--path", "a=."],
        "`a` is given to both --path and --git",
    );
}

#[test]
fn an_out_that_is_not_a_directory_fails() {
    fails_mentioning(&["a", "--out", "Cargo.toml"], "is not a directory");
}
