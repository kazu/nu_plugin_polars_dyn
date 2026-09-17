//! A scan source for `.rows` files: one value per line, read into a single column `line`.
//!
//! It exists so the `nu-polars-dyn-build` integration test has a crate to compile in that is
//! not the plugin itself. `--opts` takes `{skip: <n>}` to drop leading lines.

use nu_plugin_polars::scan::{ScanSource, parse_opts};
use polars::prelude::{
    Column, DataFrame, IntoLazy, LazyFrame, PlSmallStr, PolarsError, PolarsResult,
};
use serde::Deserialize;

/// The entry point every crate `nu-polars-dyn-build` compiles in has to expose.
pub fn scan_sources() -> &'static [&'static dyn ScanSource] {
    &[&Rows]
}

struct Rows;

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RowsOpts {
    skip: usize,
}

impl ScanSource for Rows {
    fn name(&self) -> &'static str {
        "rows"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".rows"]
    }

    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let opts: RowsOpts = serde_json::from_value(serde_json::Value::Object(parse_opts(opts)?))
            .map_err(|e| PolarsError::ComputeError(format!("opts: {e}").into()))?;
        let text = std::fs::read_to_string(source)
            .map_err(|e| PolarsError::ComputeError(format!("{source}: {e}").into()))?;
        let lines: Vec<&str> = text.lines().skip(opts.skip).collect();
        let column = Column::new(PlSmallStr::from_static("line"), &lines);
        DataFrame::new(lines.len(), vec![column]).map(IntoLazy::lazy)
    }
}
