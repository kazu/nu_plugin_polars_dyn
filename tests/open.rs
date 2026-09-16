//! `polars_dyn open` reads the built-in formats back as a LazyFrame and reports unknown suffixes
//! with the registered names. Runs the `nu` on `PATH` against the built plugin binary.

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

const TABLE: &str = "[[a b]; [1 2] [3 4]]";
/// `TABLE` as `to nuon` prints it.
const NUON: &str = "[[a, b]; [1, 2], [3, 4]]";

/// Writes `TABLE` with `polars_dyn save`, opens it again and returns the frame as nuon.
fn round_trip(file_name: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(file_name).display().to_string();
    run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ))
}

#[test]
fn opens_parquet() {
    assert_eq!(round_trip("x.parquet").trim(), NUON);
}

#[test]
fn opens_csv() {
    assert_eq!(round_trip("x.csv").trim(), NUON);
}

#[test]
fn opens_arrow() {
    assert_eq!(round_trip("x.arrow").trim(), NUON);
}

#[test]
fn opens_ndjson() {
    assert_eq!(round_trip("x.ndjson").trim(), NUON);
}

#[test]
fn open_returns_a_lazyframe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.parquet").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} | describe"
    ));
    assert_eq!(out.trim(), "polars_lazyframe");
}

#[test]
fn csv_opts_reach_the_polars_options() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} --opts {{has_header: false}} | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(
        out.trim(),
        r#"[["column_1", "column_2"]; [a, b], ["1", "2"], ["3", "4"]]"#
    );
}

#[test]
fn format_flag_overrides_the_suffix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let csv = dir.path().join("x.csv").display().to_string();
    let txt = dir.path().join("x.txt").display().to_string();
    let out = run_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {csv}; mv {csv} {txt}; \
         polars_dyn open {txt} --format csv | polars_dyn collect | polars_dyn into-nu | to nuon"
    ));
    assert_eq!(out.trim(), NUON);
}

#[test]
fn unknown_suffix_lists_the_registered_names() {
    let stderr = fail_nu("polars_dyn open data.xyz");
    assert!(
        stderr.contains("registered: parquet, csv, ipc, ndjson"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn unknown_format_lists_the_registered_names() {
    let stderr = fail_nu("polars_dyn open data.xyz --format avro");
    assert!(
        stderr.contains("registered: parquet, csv, ipc, ndjson"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn unknown_opt_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv").display().to_string();
    let stderr = fail_nu(&format!(
        "{TABLE} | polars_dyn into-df | polars_dyn save {path}; \
         polars_dyn open {path} --opts {{has_hedar: false}} | polars_dyn collect"
    ));
    assert!(
        stderr.contains("unknown option `has_hedar`"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn url_with_a_scheme_is_passed_through_untouched() {
    let stderr = fail_nu("polars_dyn open ssh://u@h/data.xyz");
    assert!(
        stderr.contains("No scan source for `ssh://u@h/data.xyz`"),
        "stderr:\n{stderr}"
    );
}
