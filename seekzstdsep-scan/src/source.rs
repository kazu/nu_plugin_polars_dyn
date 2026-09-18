//! The `.seek.zst` sources for csv and ndjson.
//!
//! `--opts` is the format's own option struct at the top level, as the plain source takes it, less
//! the options whose subject is the file — the rows to take from it, the numbering of those rows,
//! the columns to take, and the names to give them. The frames are read one at a time by readers
//! that share the options, so those would be applied once per frame, and passing one is an error
//! rather than a different answer. `slice`, `select` and `rename` take the place of three of them;
//! numbering the rows of the file has no equivalent here.
//!
//! The rest give the answer the plain source gives, with two things to know. An option that asks
//! how much of the file to look at sees only the first frame, since that is where the schema is
//! settled: `infer_schema_length` is the one of those, and it differs from the plain source
//! without saying so; [`crate::seek_zst`] has the whole of what the schema being settled there
//! costs. And ndjson's `chunk_size`, `low_memory` and `n_threads` say how polars' own reader is to
//! work through a file, which is not how a frame is read, so they have no effect here; they change
//! no answer, so they are taken rather than refused.
//!
//! The `scan` key that the plain sources accept is not taken here either. Most of what
//! `UnifiedScanArgs` names is polars' own file scan — `glob`, `cloud_options`, `hive_options` and
//! the rest never reach an anonymous scan — and the fields that do reach one are the query's to
//! fill: `pre_slice` arrives as `n_rows`, `projection` as the columns, and the schema is settled
//! from the first frame. So there is nothing for `--opts` to set here.
//!
//! One key is added beside the format's options. `verify_frames: false` reads the frames without
//! the check [`SeekZstScan`] makes by default; read on one thread, the check costs a few percent.
//!
//! ```nu
//! polars_dyn open events.jsonl.seek.zst
//! polars_dyn open data.csv.seek.zst --opts {has_header: false}
//! polars_dyn open events.jsonl.seek.zst --opts {verify_frames: false}
//! ```

use std::io::Cursor;
use std::path::PathBuf;

use polars::prelude::{DataFrame, LazyFrame, PolarsResult, SchemaRef, SerReader, polars_bail};
use polars_io::csv::read::CsvReadOptions;
use polars_io::ndjson;
use polars_io::utils::overwrite_schema;

use crate::seek_zst::{FrameParser, SeekZstScan};
use nu_plugin_polars::scan::{ScanSource, builtin::ndjson_defaults, overlay_opts};
use polars_plan::dsl::NDJsonReadOptions;
use serde_json::Value;

pub struct CsvSeekZst;
pub struct NdJsonSeekZst;

impl ScanSource for CsvSeekZst {
    fn name(&self) -> &'static str {
        "csv-seek-zst"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".csv.seek.zst"]
    }

    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, verify_frames) = format_opts(CsvReadOptions::default(), opts)?;
        reject_options_the_frames_cannot_honour(&options)?;
        SeekZstScan::lazy_frame_with(
            PathBuf::from(source),
            Box::new(CsvFrames { options }),
            verify_frames,
        )
    }
}

/// Rejects the csv options whose subject is the file: the rows to take from it, the numbering of
/// those rows, the columns to take, and the names to give them.
///
/// Every frame is read by its own reader holding these same options, so each would be applied once
/// per frame: `n_rows: 3` would keep three rows of each frame, `row_index` would start again at
/// every frame, `columns` would leave the scan's schema narrower than the frames it is read with,
/// and `column_names_overwrite` would be overridden by the schema the frames are read with.
///
/// The options this leaves alone are the ones that say how to read a line, plus the ones whose
/// meaning a frame does not change. `infer_schema_length` is neither, and is still taken: it is
/// bounded by the first frame like the rest of the schema, which the module doc says.
fn reject_options_the_frames_cannot_honour(options: &CsvReadOptions) -> PolarsResult<()> {
    let rows = [
        ("n_rows", options.n_rows.is_some()),
        ("skip_rows", options.skip_rows != 0),
        ("skip_lines", options.skip_lines != 0),
        (
            "skip_rows_after_header",
            options.skip_rows_after_header != 0,
        ),
    ];
    if let Some((name, _)) = rows.iter().find(|(_, set)| *set) {
        polars_bail!(
            ComputeError:
            "`{name}` picks rows out of the file, which a frame-at-a-time scan cannot honour; \
             use `polars_dyn slice` instead"
        )
    }

    if options.row_index.is_some() {
        polars_bail!(
            ComputeError:
            "`row_index` numbers the rows of the file, which a frame-at-a-time scan cannot \
             honour: the count would start again at every frame"
        )
    }

    let columns = [
        ("columns", options.columns.is_some()),
        ("projection", options.projection.is_some()),
    ];
    if let Some((name, _)) = columns.iter().find(|(_, set)| *set) {
        polars_bail!(
            ComputeError:
            "`{name}` picks columns out of the file, which a frame-at-a-time scan cannot honour; \
             use `polars_dyn select` instead"
        )
    }

    if options.column_names_overwrite.is_some() {
        polars_bail!(
            ComputeError:
            "`column_names_overwrite` names the columns of the file, which the frames take from \
             the schema instead; use `polars_dyn rename`"
        )
    }
    Ok(())
}

impl ScanSource for NdJsonSeekZst {
    fn name(&self) -> &'static str {
        "ndjson-seek-zst"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".ndjson.seek.zst", ".jsonl.seek.zst"]
    }

    fn scan(&self, source: &str, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, verify_frames) = format_opts(ndjson_defaults(), opts)?;
        SeekZstScan::lazy_frame_with(
            PathBuf::from(source),
            Box::new(NdJsonFrames { options }),
            verify_frames,
        )
    }
}

struct CsvFrames {
    options: CsvReadOptions,
}

impl FrameParser for CsvFrames {
    fn schema(&self, frame: &[u8]) -> PolarsResult<SchemaRef> {
        Ok(self
            .options
            .clone()
            .into_reader_with_file_handle(Cursor::new(frame))
            .finish()?
            .schema()
            .clone())
    }

    /// The header line is in frame 0 only, so every later frame is read without one and takes its
    /// column names from the schema instead.
    fn parse(&self, frame: &[u8], index: usize, schema: &SchemaRef) -> PolarsResult<DataFrame> {
        self.options
            .clone()
            .with_has_header(self.options.has_header && index == 0)
            .with_schema(Some(schema.clone()))
            .into_reader_with_file_handle(Cursor::new(frame))
            .finish()
    }
}

struct NdJsonFrames {
    options: NDJsonReadOptions,
}

impl FrameParser for NdJsonFrames {
    /// The schema polars would settle on for this text: `schema` when it was given, and otherwise
    /// what the frame infers with `schema_overwrite` laid over it.
    ///
    /// A given `schema` is taken as it is, `schema_overwrite` and all, because that is what the
    /// plain source answers with. The laying over is `polars_io`'s own, so that naming a column the
    /// file does not have is the error it is there rather than a line quietly ignored.
    fn schema(&self, frame: &[u8]) -> PolarsResult<SchemaRef> {
        if let Some(schema) = &self.options.schema {
            return Ok(schema.clone());
        }
        let mut cursor = Cursor::new(frame);
        let mut schema = ndjson::infer_schema(&mut cursor, self.options.infer_schema_length)?;
        if let Some(overwrite) = &self.options.schema_overwrite {
            overwrite_schema(&mut schema, overwrite)?;
        }
        Ok(SchemaRef::new(schema))
    }

    fn parse(&self, frame: &[u8], _index: usize, schema: &SchemaRef) -> PolarsResult<DataFrame> {
        ndjson::core::parse_ndjson(frame, None, schema, self.options.ignore_errors)
    }
}

/// The one key `--opts` holds beside the format's options: `false` reads the frames without
/// checking their record counts.
const VERIFY_FRAMES: &str = "verify_frames";

/// Deserializes the format's options from `--opts`, and whether to check the record counts of the
/// frames, which is the default.
fn format_opts<T>(defaults: T, opts: &[u8]) -> PolarsResult<(T, bool)>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let mut opts = nu_plugin_polars::scan::parse_opts(opts)?;
    let verify_frames = match opts.remove(VERIFY_FRAMES) {
        None => true,
        Some(Value::Bool(verify)) => verify,
        Some(other) => polars_bail!(ComputeError: "`{VERIFY_FRAMES}` takes a bool, not {other}"),
    };
    Ok((overlay_opts(defaults, &Value::Object(opts))?, verify_frames))
}
