//! The `.seek.zst` scan source for `nu_plugin_polars_dyn`: a seekable zstd file as the records it
//! decompresses to.
//!
//! Build a plugin with it compiled in:
//!
//! ```nu
//! nu-polars-dyn-build seekzstdsep_scan --path seekzstdsep_scan=./seekzstdsep-scan
//! ```
//!
//! It wraps: the source `events.jsonl.seek.zst` is the chain `file`, `seek-zst`, `ndjson`, and
//! whatever follows `seek-zst` reads the decompressed records without knowing they were
//! compressed. What a `.seek.zst` buys over the plain text file is disk: the bytes read shrink with
//! the compression ratio while the file stays appendable. It is not a faster reader.
//!
//! ```nu
//! polars_dyn open events.jsonl.seek.zst
//! polars_dyn open data.csv.seek.zst --opts {csv: {has_header: false}}
//! polars_dyn open dump.bin --format seek-zst,ndjson
//! polars_dyn open events.jsonl.seek.zst --opts {seek-zst: {record_filter: "error"}}
//! ```
//!
//! `--opts` under `seek-zst`, every field optional and an unknown key an error:
//!
//! - `finder`, `finder_arg`: where a record ends, as seekzstdsep's `--finder` and `--finder-arg`
//!   name it. The default is `sep` with `"\n"`. The parsers after it read lines, so a boundary
//!   other than a newline suits only a source that reads its records some other way.
//! - `verify_frames` (default `true`): checks that every frame holds the record count seekzstdsep
//!   writes, and fails the read at the first one that does not.
//! - `record_filter`: keeps only the records holding these bytes, before any parser sees them. A
//!   `first` or `slice 0 n` counts the records it keeps, and a csv header line is a record like
//!   any other.

pub mod seek_zst;

use nu_plugin_polars::scan::ScanSource;

/// The entry point `nu-polars-dyn-build` calls from the `main.rs` it generates.
pub fn scan_sources() -> &'static [&'static dyn ScanSource] {
    &[&seek_zst::SeekZst]
}
