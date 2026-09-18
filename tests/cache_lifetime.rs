//! A cached value stays alive as long as nushell holds a handle to it, and `polars_dyn store-rm`
//! removes one regardless. Runs the `nu` on `PATH` against the built plugin binary.

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
        "let lf = ([[a b];[1 2] [3 4]] | polars_dyn into-lazy); \
         $lf | polars_dyn select a | ignore; \
         $lf | polars_dyn collect | polars_dyn into-nu | to nuon",
    );
    assert_eq!(out.trim(), "[[a, b]; [1, 2], [3, 4]]");
}

#[test]
fn dataframe_survives_a_dropped_copy_with_the_same_id() {
    let out = run_nu(
        "let df = ([[a b];[1 2] [3 4]] | polars_dyn into-df); \
         $df | polars_dyn into-df | ignore; \
         $df | polars_dyn into-nu | to nuon",
    );
    assert_eq!(out.trim(), "[[a, b]; [1, 2], [3, 4]]");
}

#[test]
fn a_repeated_pipeline_does_not_grow_the_store() {
    let out = run_nu(
        "for _ in 1..20 { [[a b];[1 2] [3 4]] | polars_dyn into-lazy | polars_dyn collect | ignore }; \
         polars_dyn store-ls | length",
    );
    assert_eq!(out.trim(), "0");
}

#[test]
fn store_rm_removes_the_value() {
    let out = run_nu(
        "let df = ([[a b];[1 2] [3 4]] | polars_dyn into-df); \
         polars_dyn store-ls | get key | first | polars_dyn store-rm $in | ignore; \
         polars_dyn store-ls | length",
    );
    assert_eq!(out.trim(), "0");
}
