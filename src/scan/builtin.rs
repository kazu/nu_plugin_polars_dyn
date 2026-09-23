//! The sources every bin registers: parquet, csv, ipc and ndjson, the four polars reads by
//! itself.
//!
//! Their `--opts` record is the format's own option struct at the top level, with polars'
//! `UnifiedScanArgs` (`cloud_options`, `hive_options`, `glob`, `row_index`, `pre_slice`, ...)
//! under the key `scan`. Both are polars' serde forms, so a `u8` such as the csv separator is a
//! number:
//!
//! ```nu
//! polars_dyn open data.csv --opts {has_header: false, parse_options: {separator: 59}, scan: {glob: false}}
//! ```

use std::num::NonZeroUsize;

use polars::prelude::{LazyFrame, PlRefPath, PolarsResult};
use polars_io::{csv::read::CsvReadOptions, ipc::IpcScanOptions, prelude::ParquetOptions};
use polars_plan::dsl::{
    DslBuilder, DslPlan, FileScanDsl, NDJsonReadOptions, ScanSources, UnifiedScanArgs,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use super::opts::{overlay_opts, parse_opts};

/// A source polars reads by itself from a path or URL: globs, cloud storage and its own parallel
/// readers come with it, which is why it takes the string instead of a [`ReadAt`](super::ReadAt).
pub struct Builtin {
    /// The registered name, matched against `--format`.
    pub name: &'static str,
    /// Matched against the end of the source string when `--format` is absent.
    pub suffixes: &'static [&'static str],
    /// Builds the `LazyFrame` over an absolute local path or a URL, with the `--opts` record as
    /// JSON bytes.
    pub scan: fn(&str, &[u8]) -> PolarsResult<LazyFrame>,
}

/// The sources every bin has.
pub static BUILTIN: &[Builtin] = &[
    Builtin {
        name: "parquet",
        suffixes: &[".parquet", ".parq", ".pq"],
        scan: parquet,
    },
    Builtin {
        name: "csv",
        suffixes: &[".csv"],
        scan: csv,
    },
    Builtin {
        name: "ipc",
        suffixes: &[".arrow", ".ipc"],
        scan: ipc,
    },
    Builtin {
        name: "ndjson",
        suffixes: &[".ndjson", ".jsonl"],
        scan: ndjson,
    },
];

fn parquet(source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
    let (options, scan_args) = split_opts(ParquetOptions::default(), opts)?;
    Ok(
        DslBuilder::scan_parquet(sources(source), options, scan_args)?
            .build()
            .into(),
    )
}

fn csv(source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
    let (options, scan_args) = split_opts(CsvReadOptions::default(), opts)?;
    Ok(DslBuilder::scan_csv(sources(source), options, scan_args)?
        .build()
        .into())
}

fn ipc(source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
    let (options, scan_args) = split_opts(IpcScanOptions::default(), opts)?;
    Ok(DslBuilder::scan_ipc(sources(source), options, scan_args)?
        .build()
        .into())
}

fn ndjson(source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
    let (options, scan_args) = split_opts(ndjson_defaults(), opts)?;
    Ok(DslPlan::Scan {
        sources: sources(source),
        unified_scan_args: Box::new(scan_args),
        scan_type: Box::new(FileScanDsl::NDJson { options }),
        cached_ir: Default::default(),
    }
    .into())
}

/// The values `LazyJsonLineReader::new` starts from. `NDJsonReadOptions` has no `Default` and
/// polars offers no builder from it to a plan, so the defaults are repeated here.
pub fn ndjson_defaults() -> NDJsonReadOptions {
    const CHUNK_SIZE: NonZeroUsize = NonZeroUsize::new(1 << 18).expect("1 << 18 is not zero");
    NDJsonReadOptions {
        n_threads: None,
        infer_schema_length: NonZeroUsize::new(100),
        chunk_size: CHUNK_SIZE,
        low_memory: false,
        ignore_errors: false,
        schema: None,
        schema_overwrite: None,
    }
}

/// Splits the `--opts` record into the format's options (top level) and the `UnifiedScanArgs`
/// under `scan`, each overlaid on its default.
fn split_opts<T>(defaults: T, opts: &[u8]) -> PolarsResult<(T, UnifiedScanArgs)>
where
    T: Serialize + DeserializeOwned,
{
    let mut record = parse_opts(opts)?;
    let scan = record
        .remove("scan")
        .unwrap_or_else(|| Value::Object(Map::new()));
    let scan_args = overlay_opts(UnifiedScanArgs::default(), &scan)?;
    let options = overlay_opts(defaults, &Value::Object(record))?;
    Ok((options, scan_args))
}

fn sources(source: &str) -> ScanSources {
    ScanSources::Paths(std::iter::once(PlRefPath::from(source)).collect())
}
