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

fn run_nu(script: &str, verbose: bool) -> (String, String) {
    let output = nu(script, verbose);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "nu failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

/// Writes a csv the filter keeps two rows of, then collects the same query on the given engine.
fn filtered_csv(flag: &str, verbose: bool) -> (String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.csv").display().to_string();
    run_nu(
        &format!(
            "[[a b]; [1 2] [3 4] [5 6]] | polars_dyn into-df | polars_dyn save {path}; \
             polars_dyn open {path} | polars_dyn filter ((polars_dyn col a) > 1) \
             | polars_dyn first 1 | polars_dyn collect {flag} | polars_dyn into-nu | to nuon"
        ),
        verbose,
    )
}

#[test]
fn streaming_returns_the_same_frame_as_in_memory() {
    let (streaming, _) = filtered_csv("--streaming", false);
    let (in_memory, _) = filtered_csv("", false);
    assert_eq!(streaming.trim(), "[[a, b]; [3, 4]]");
    assert_eq!(streaming, in_memory);
}

/// With the `streaming` feature on, the in-memory engine also hands file scans to polars-stream,
/// so `polars-stream:` lines appear on both engines. What tells them apart is the node that runs
/// the slice: the in-memory executor logs `run sink_mem` / `run SliceExec`, the streaming engine
/// logs `running streaming-slice in subgraph`.
#[test]
fn streaming_flag_selects_the_streaming_engine() {
    let (_, streaming) = filtered_csv("--streaming", true);
    assert!(
        streaming.contains("polars-stream: running streaming-slice in subgraph"),
        "stderr:\n{streaming}"
    );
    assert!(!streaming.contains("run sink_mem"), "stderr:\n{streaming}");

    let (_, in_memory) = filtered_csv("", true);
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
