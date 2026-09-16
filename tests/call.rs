//! `polars_dyn call` runs a function from a polars expression plugin (`tests/expr_plugin`,
//! built as a workspace member next to the plugin binary). Runs the `nu` on `PATH` against the
//! built plugin binary.

use std::path::Path;
use std::process::{Command, Output};

fn nu(script: &str) -> Output {
    let plugin = env!("CARGO_BIN_EXE_nu_plugin_polars_dyn");
    Command::new("nu")
        .args([
            "--no-config-file",
            "--plugins",
            &format!("[{plugin}]"),
            "-c",
            script,
        ])
        .output()
        .expect("`nu` must be on PATH to run the integration tests")
}

fn run_nu(script: &str) -> String {
    let output = nu(script);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "nu failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

fn fail_nu(script: &str) -> String {
    let output = nu(script);
    assert!(!output.status.success(), "nu succeeded unexpectedly");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The `.so` of `tests/expr_plugin`, which `cargo test --workspace` builds into the same
/// directory as the plugin binary.
fn plugin_so() -> String {
    let so = Path::new(env!("CARGO_BIN_EXE_nu_plugin_polars_dyn"))
        .parent()
        .expect("binary has a parent directory")
        .join("libexpr_plugin.so");
    assert!(
        so.exists(),
        "{} is missing: build with `cargo test --workspace`",
        so.display()
    );
    so.display().to_string()
}

const TABLE: &str = "[[a]; [1] [2] [3]]";

#[test]
fn calls_add_with_kwargs() {
    let so = plugin_so();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df \
         | polars_dyn select (polars_dyn call {so} add a --kwargs {{n: 10}} --elementwise) \
         | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(out.trim(), "[[a]; [11], [12], [13]]");
}

#[test]
fn resolves_relative_path_before_collect() {
    let so = plugin_so();
    let dir = Path::new(&so).parent().expect("so has a parent directory");
    let out = run_nu(&format!(
        "cd {}; let e = (polars_dyn call libexpr_plugin.so add a --kwargs {{n: 1}}); cd /; \
         {TABLE} | polars_dyn into-df | polars_dyn select $e \
         | polars_dyn collect | polars_dyn into-nu | to nuon",
        dir.display()
    ));
    assert_eq!(out.trim(), "[[a]; [2], [3], [4]]");
}

#[test]
fn reports_missing_library() {
    let dir = tempfile::tempdir().expect("tempdir");
    let so = dir.path().join("libnothing.so").display().to_string();
    let err = fail_nu(&format!(
        "{TABLE} | polars_dyn into-df \
         | polars_dyn select (polars_dyn call {so} add a) | polars_dyn collect"
    ));
    assert!(
        err.contains("error loading dynamic library"),
        "unexpected error:\n{err}"
    );
}

#[test]
fn reports_plugin_error() {
    let so = plugin_so();
    let err = fail_nu(&format!(
        "{TABLE} | polars_dyn into-df \
         | polars_dyn select (polars_dyn call {so} add a --kwargs {{m: 1}}) | polars_dyn collect"
    ));
    assert!(
        err.contains("could not parse kwargs"),
        "unexpected error:\n{err}"
    );
}

#[test]
fn rejects_unsupported_kwargs_value() {
    let so = plugin_so();
    let err = fail_nu(&format!("polars_dyn call {so} add a --kwargs {{n: 1sec}}"));
    assert!(err.contains("--kwargs expects"), "unexpected error:\n{err}");
}
