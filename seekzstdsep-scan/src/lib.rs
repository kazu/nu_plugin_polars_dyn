//! The `.csv.seek.zst` / `.ndjson.seek.zst` / `.jsonl.seek.zst` scan sources for
//! `nu_plugin_polars_dyn`.
//!
//! Build a plugin with them compiled in:
//!
//! ```nu
//! nu-polars-dyn-build seekzstdsep_scan --path seekzstdsep_scan=./seekzstdsep-scan
//! ```
//!
//! [`seek_zst`] reads a seekable zstd file one frame at a time; [`source`] puts polars' own csv
//! and ndjson readers on top of it. What the sources take in `--opts`, and where their answers
//! differ from the plain file, is in those two modules.

pub mod seek_zst;
pub mod source;

use nu_plugin_polars::scan::ScanSource;

/// The entry point `nu-polars-dyn-build` calls from the `main.rs` it generates.
pub fn scan_sources() -> &'static [&'static dyn ScanSource] {
    &[&source::CsvSeekZst, &source::NdJsonSeekZst]
}
