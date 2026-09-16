//! Cached values stay alive until `polars store-rm` or the end of the plugin process.
//! Runs the `nu` on `PATH` against the built plugin binary.

use std::process::Command;

fn run_nu(script: &str) -> String {
    let plugin = env!("CARGO_BIN_EXE_nu_plugin_polars_dyn");
    let output = Command::new("nu")
        .args([
            "--no-config-file",
            "--plugins",
            &format!("[{plugin}]"),
            "-c",
            script,
        ])
        .output()
        .expect("`nu` must be on PATH to run the integration tests");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "nu failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

#[test]
fn lazyframe_in_a_variable_collects_later() {
    let out = run_nu(
        "let lf = ([[a b];[1 2] [3 4]] | polars into-lazy); \
         $lf | polars select a | ignore; \
         $lf | polars collect | polars into-nu | to nuon",
    );
    assert_eq!(out.trim(), "[[a, b]; [1, 2], [3, 4]]");
}

#[test]
fn dataframe_survives_a_dropped_copy_with_the_same_id() {
    let out = run_nu(
        "let df = ([[a b];[1 2] [3 4]] | polars into-df); \
         $df | polars into-df | ignore; \
         $df | polars into-nu | to nuon",
    );
    assert_eq!(out.trim(), "[[a, b]; [1, 2], [3, 4]]");
}

#[test]
fn store_rm_removes_the_value() {
    let out = run_nu(
        "let df = ([[a b];[1 2] [3 4]] | polars into-df); \
         polars store-ls | get key | first | polars store-rm $in | ignore; \
         polars store-ls | length",
    );
    assert_eq!(out.trim(), "0");
}
