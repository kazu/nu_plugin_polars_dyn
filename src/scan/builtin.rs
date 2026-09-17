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

use super::{
    ScanSource,
    opts::{overlay_opts, parse_opts},
};

/// The sources every bin registers.
pub static BUILTIN: &[&dyn ScanSource] = &[&Parquet, &Csv, &Ipc, &NdJson];

pub struct Parquet;
pub struct Csv;
pub struct Ipc;
pub struct NdJson;

impl ScanSource for Parquet {
    fn name(&self) -> &'static str {
        "parquet"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".parquet", ".parq", ".pq"]
    }

    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, scan_args) = split_opts(ParquetOptions::default(), opts)?;
        Ok(
            DslBuilder::scan_parquet(sources(source), options, scan_args)?
                .build()
                .into(),
        )
    }
}

impl ScanSource for Csv {
    fn name(&self) -> &'static str {
        "csv"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".csv"]
    }

    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, scan_args) = split_opts(CsvReadOptions::default(), opts)?;
        Ok(DslBuilder::scan_csv(sources(source), options, scan_args)?
            .build()
            .into())
    }
}

impl ScanSource for Ipc {
    fn name(&self) -> &'static str {
        "ipc"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".arrow", ".ipc"]
    }

    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, scan_args) = split_opts(IpcScanOptions::default(), opts)?;
        Ok(DslBuilder::scan_ipc(sources(source), options, scan_args)?
            .build()
            .into())
    }
}

impl ScanSource for NdJson {
    fn name(&self) -> &'static str {
        "ndjson"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".ndjson", ".jsonl"]
    }

    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, scan_args) = split_opts(ndjson_defaults(), opts)?;
        Ok(DslPlan::Scan {
            sources: sources(source),
            unified_scan_args: Box::new(scan_args),
            scan_type: Box::new(FileScanDsl::NDJson { options }),
            cached_ir: Default::default(),
        }
        .into())
    }
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
