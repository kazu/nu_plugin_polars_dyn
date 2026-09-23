//! `polars_dyn collect --streaming` runs the query on the polars streaming engine and returns
//! the same frame as the in-memory engine. Sources built on `AnonymousScan` cannot run there.
//! Runs the `nu` on `PATH` against the built plugin binary.

use std::any::Any;
use std::process::{Command, Output};
use std::sync::Arc;

use nu_plugin_polars::values::NuLazyFrame;
use nu_protocol::{ShellError, Span};
use polars::prelude::{
    AnonymousScan, AnonymousScanArgs, DataFrame, Engine, LazyFrame, PolarsResult,
    ScanArgsAnonymous, Schema, SchemaRef, df,
};

fn nu(script: &str, verbose: bool) -> Output {
    let plugin = env!("CARGO_BIN_EXE_nu_plugin_polars_dyn");
    let mut command = Command::new("nu");
    command.args([
        "--no-config-file",
        "--plugins",
        &format!("[{plugin}]"),
        "-c",
        script,
    ]);
    if verbose {
        command.env("POLARS_VERBOSE", "1");
    }
    command
        .output()
        .expect("`nu` must be on PATH to run the integration tests")
}

/// Writes a file the filter keeps two rows of, then collects the same query on the given engine.
/// parquet is polars' own scan over the bytes, which the streaming engine runs; csv and ndjson
/// are the plugin's chunked scan, an `AnonymousScan`, which it does not.
fn filtered(file_name: &str, flag: &str, verbose: bool) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(file_name).display().to_string();
    nu(
        &format!(
            "[[a b]; [1 2] [3 4] [5 6]] | polars_dyn into-df | polars_dyn save {path}; \
             polars_dyn open {path} | polars_dyn filter ((polars_dyn col a) > 1) \
             | polars_dyn first 1 | polars_dyn collect {flag} | polars_dyn into-nu | to nuon"
        ),
        verbose,
    )
}

fn filtered_parquet(flag: &str, verbose: bool) -> (String, String) {
    let output = filtered("x.parquet", flag, verbose);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "nu failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

#[test]
fn streaming_returns_the_same_frame_as_in_memory() {
    let (streaming, _) = filtered_parquet("--streaming", false);
    let (in_memory, _) = filtered_parquet("", false);
    assert_eq!(streaming.trim(), "[[a, b]; [3, 4]]");
    assert_eq!(streaming, in_memory);
}

/// With the `streaming` feature on, the in-memory engine also hands file scans to polars-stream,
/// so `polars-stream:` lines appear on both engines. What tells them apart is the node that runs
/// the slice: the in-memory executor logs `run sink_mem` / `run SliceExec`, the streaming engine
/// logs `running streaming-slice in subgraph`.
#[test]
fn streaming_flag_selects_the_streaming_engine() {
    let (_, streaming) = filtered_parquet("--streaming", true);
    assert!(
        streaming.contains("polars-stream: running streaming-slice in subgraph"),
        "stderr:\n{streaming}"
    );
    assert!(!streaming.contains("run sink_mem"), "stderr:\n{streaming}");

    let (_, in_memory) = filtered_parquet("", true);
    assert!(in_memory.contains("run sink_mem"), "stderr:\n{in_memory}");
    assert!(
        !in_memory.contains("running streaming-slice"),
        "stderr:\n{in_memory}"
    );
}

/// A two-row frame served through `LazyFrame::anonymous_scan`, the shape a registry source
/// takes when it is not one of polars' file scans.
struct TwoRows;

impl AnonymousScan for TwoRows {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn scan(&self, _scan_opts: AnonymousScanArgs) -> PolarsResult<DataFrame> {
        df!("a" => [1i64, 3], "b" => [2i64, 4])
    }

    fn schema(&self, _infer_schema_length: Option<usize>) -> PolarsResult<SchemaRef> {
        self.scan(AnonymousScanArgs {
            n_rows: None,
            with_columns: None,
            schema: Arc::new(Schema::default()),
            output_schema: None,
            predicate: None,
        })
        .map(|df| Arc::new(df.schema().as_ref().clone()))
    }
}

fn anonymous_frame() -> NuLazyFrame {
    let lazy = LazyFrame::anonymous_scan(Arc::new(TwoRows), ScanArgsAnonymous::default())
        .expect("anonymous_scan with a schema function must succeed");
    NuLazyFrame::new(false, lazy)
}

#[test]
fn anonymous_scan_collects_in_memory() {
    let df = anonymous_frame()
        .collect(Span::test_data())
        .expect("in-memory collect of an anonymous scan");
    assert_eq!(df.as_ref().height(), 2);
}

/// polars-stream has no `AnonymousScan` node and stops with `todo!`; the plugin reports that
/// panic as an error instead of falling back to the in-memory engine.
#[test]
fn anonymous_scan_is_an_error_on_the_streaming_engine() {
    let err = anonymous_frame()
        .collect_with_engine(Engine::Streaming, Span::test_data())
        .expect_err("polars-stream 0.55.2 does not implement AnonymousScan");
    let ShellError::Generic(err) = err else {
        panic!("expected a generic error, got {err:?}");
    };
    assert_eq!(err.error, "Panic occurred");
    assert_eq!(
        err.msg,
        "collecting on the streaming engine: not yet implemented: unimplemented: AnonymousScan"
    );
}

/// The built-in csv and ndjson are the plugin's chunked scan, an `AnonymousScan`, so they fail on
/// the streaming engine the way a compiled-in source does.
#[test]
fn csv_and_ndjson_cannot_collect_on_the_streaming_engine() {
    for file_name in ["x.csv", "x.ndjson"] {
        let output = filtered(file_name, "--streaming", false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{file_name} collected:\n{stderr}");
        assert!(stderr.contains("AnonymousScan"), "{file_name}:\n{stderr}");
    }
}
