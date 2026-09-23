//! The `.seek.zst` scan source for `nu_plugin_polars_dyn`: a seekable zstd file as the bytes it
//! decompresses to.
//!
//! Build a plugin with it compiled in:
//!
//! ```nu
//! nu-polars-dyn-build seekzstdsep_scan --path seekzstdsep_scan=./seekzstdsep-scan
//! ```
//!
//! It wraps: the source `events.jsonl.seek.zst` is the chain `file`, `seek-zst`, `ndjson`, and
//! whatever follows `seek-zst` reads the decompressed bytes without knowing they were compressed.
//! What a `.seek.zst` buys over the plain text file is disk: the bytes read shrink with the
//! compression ratio while the file stays appendable. It is not a faster reader.
//!
//! ```nu
//! polars_dyn open events.jsonl.seek.zst
//! polars_dyn open data.csv.seek.zst --opts {csv: {has_header: false}}
//! polars_dyn open dump.bin --format seek-zst,ndjson
//! ```
//!
//! `seek-zst` takes no options.

pub mod seek_zst;

use nu_plugin_polars::scan::ScanSource;

/// The entry point `nu-polars-dyn-build` calls from the `main.rs` it generates.
pub fn scan_sources() -> &'static [&'static dyn ScanSource] {
    &[&seek_zst::SeekZst]
}
