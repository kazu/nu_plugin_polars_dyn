//! A scan source for `.rows` files: one value per line, read into a single column `line`.
//!
//! It exists so the `nu-polars-dyn-build` integration test has a crate to compile in that is
//! not the plugin itself. `--opts` takes `{skip: <n>}` to drop leading lines.

use std::sync::Arc;

use nu_plugin_polars::scan::{ReadAt, ScanSource, parse_opts};
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

    fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let opts: RowsOpts = serde_json::from_value(serde_json::Value::Object(parse_opts(opts)?))
            .map_err(|e| PolarsError::ComputeError(format!("opts: {e}").into()))?;
        let text = read_to_string(&*source)
            .map_err(|e| PolarsError::ComputeError(format!("rows: {e}").into()))?;
        let lines: Vec<&str> = text.lines().skip(opts.skip).collect();
        let column = Column::new(PlSmallStr::from_static("line"), &lines);
        DataFrame::new(lines.len(), vec![column]).map(IntoLazy::lazy)
    }
}

/// The whole of `source` as text.
fn read_to_string(source: &dyn ReadAt) -> std::io::Result<String> {
    let len = usize::try_from(source.len()?).map_err(std::io::Error::other)?;
    let mut bytes = vec![0; len];
    let mut filled = 0;
    while filled < len {
        let n = source.read_at(filled as u64, &mut bytes[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    bytes.truncate(filled);
    String::from_utf8(bytes).map_err(std::io::Error::other)
}
