//! The sources every bin registers: `file`, which opens a local path, and parquet, csv, ipc and
//! ndjson, the four polars reads by itself.
//!
//! `--opts` for the four formats is the format's own option struct in polars' serde form, so a
//! `u8` such as the csv separator is a number. parquet and ipc also take polars' `UnifiedScanArgs`
//! (`row_index`, `pre_slice`, ...) under the key `scan`, since polars' own lazy scan reads them
//! from the bytes:
//!
//! ```nu
//! polars_dyn open data.parquet --opts {parquet: {scan: {row_index: {name: i, offset: 0}}}}
//! polars_dyn open data.csv --opts {csv: {has_header: false, parse_options: {separator: 59}}}
//! ```
//!
//! csv and ndjson are read by the plugin's own chunked scan instead, which cuts the bytes at
//! newlines and hands each chunk to polars' reader; `chunk_size` is the width of those chunks. A
//! newline inside a quoted csv field is a newline to the cut, and a cut there is a parse error,
//! so a csv with such fields is read with a `chunk_size` of at least its length.
//! Because every chunk is read by a reader of its own holding the same options, the options whose
//! subject is the whole file — the rows to take from it, the numbering of those rows, the columns
//! to take, and the names to give them — would be applied once per chunk, and passing one is an
//! error rather than a different answer. `slice`, `select` and `rename` take the place of three
//! of them; numbering the rows has no equivalent here. The `scan` key is not taken either: what
//! `UnifiedScanArgs` names is polars' own file scan, and the fields that do reach an anonymous
//! scan are the query's to fill. An option that asks how much of the file to look at sees only
//! the first chunk, since that is where the schema is settled: `infer_schema_length` is the one
//! of those. ndjson's `low_memory` and `n_threads` say how polars' own reader is to work through a
//! file, which is not how a chunk is read, so they have no effect here.

use std::{fs::File, io::Cursor, num::NonZeroUsize, sync::Arc};

use polars::prelude::{
    DataFrame, LazyFrame, PolarsError, PolarsResult, SchemaRef, SerReader, polars_bail,
};
use polars_buffer::Buffer;
use polars_io::{
    HiveOptions, csv::read::CsvReadOptions, ipc::IpcScanOptions, ndjson, prelude::ParquetOptions,
    utils::overwrite_schema,
};
use polars_plan::dsl::{DslBuilder, NDJsonReadOptions, ScanSources, UnifiedScanArgs};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use super::{
    ReadAt, ScanSource,
    chunked::{ChunkParser, ChunkedScan},
    no_opts,
    opts::{overlay_opts, parse_opts},
    read_fully,
};

/// The sources every bin has, `file` first.
pub static BUILTIN: &[&dyn ScanSource] = &[FILE, &Parquet, &Csv, &Ipc, &NdJson];

/// The source a string without a scheme is opened by.
pub static FILE: &dyn ScanSource = &LocalFile;

/// Scheme `file`: a local path, absolute or `file://` prefixed, opened as a `File`.
struct LocalFile;

impl ScanSource for LocalFile {
    fn name(&self) -> &'static str {
        "file"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[]
    }

    fn schemes(&self) -> &'static [&'static str] {
        &["file"]
    }

    fn open(&self, url: &str, opts: &[u8]) -> PolarsResult<Arc<dyn ReadAt>> {
        no_opts(self.name(), opts)?;
        let path = url.strip_prefix("file://").unwrap_or(url);
        let file = File::open(path)
            .map_err(|e| PolarsError::ComputeError(format!("cannot open `{path}`: {e}").into()))?;
        Ok(Arc::new(file))
    }
}

struct Parquet;
struct Csv;
struct Ipc;
struct NdJson;

impl ScanSource for Parquet {
    fn name(&self) -> &'static str {
        "parquet"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".parquet", ".parq", ".pq"]
    }

    fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, scan_args) = split_opts(ParquetOptions::default(), opts)?;
        Ok(
            DslBuilder::scan_parquet(buffers(&*source)?, options, scan_args)?
                .build()
                .into(),
        )
    }
}

impl ScanSource for Ipc {
    fn name(&self) -> &'static str {
        "ipc"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".arrow", ".ipc"]
    }

    fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let (options, scan_args) = split_opts(IpcScanOptions::default(), opts)?;
        Ok(
            DslBuilder::scan_ipc(buffers(&*source)?, options, scan_args)?
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

    fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let options = overlay_opts(CsvReadOptions::default(), &Value::Object(parse_opts(opts)?))?;
        reject_options_the_chunks_cannot_honour(&options)?;
        let chunk_size = options.chunk_size;
        ChunkedScan::lazy_frame(source, Box::new(CsvChunks { options }), chunk_size)
    }
}

impl ScanSource for NdJson {
    fn name(&self) -> &'static str {
        "ndjson"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".ndjson", ".jsonl"]
    }

    fn scan(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<LazyFrame> {
        let options = overlay_opts(ndjson_defaults(), &Value::Object(parse_opts(opts)?))?;
        let chunk_size = options.chunk_size.get();
        ChunkedScan::lazy_frame(source, Box::new(NdJsonChunks { options }), chunk_size)
    }
}

/// The whole of `source` as the one in-memory buffer polars' own lazy scan reads.
fn buffers(source: &dyn ReadAt) -> PolarsResult<ScanSources> {
    let len = usize::try_from(source.len()?)
        .map_err(|e| PolarsError::ComputeError(e.to_string().into()))?;
    let mut bytes = vec![0; len];
    let read = read_fully(source, 0, &mut bytes)?;
    bytes.truncate(read);
    Ok(ScanSources::Buffers(Arc::from([Buffer::from(bytes)])))
}

/// Rejects the csv options whose subject is the file: the rows to take from it, the numbering of
/// those rows, the columns to take, and the names to give them.
///
/// Every chunk is read by its own reader holding these same options, so each would be applied once
/// per chunk: `n_rows: 3` would keep three rows of each chunk, `row_index` would start again at
/// every chunk, `columns` would leave the scan's schema narrower than the chunks it is read with,
/// and `column_names_overwrite` would be overridden by the schema the chunks are read with.
///
/// The options this leaves alone are the ones that say how to read a line, plus the ones whose
/// meaning a chunk does not change. `infer_schema_length` is neither, and is still taken: it is
/// bounded by the first chunk like the rest of the schema, which the module doc says.
fn reject_options_the_chunks_cannot_honour(options: &CsvReadOptions) -> PolarsResult<()> {
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
            "`{name}` picks rows out of the file, which a chunk-at-a-time scan cannot honour; \
             use `polars_dyn slice` instead"
        )
    }

    if options.row_index.is_some() {
        polars_bail!(
            ComputeError:
            "`row_index` numbers the rows of the file, which a chunk-at-a-time scan cannot \
             honour: the count would start again at every chunk"
        )
    }

    let columns = [
        ("columns", options.columns.is_some()),
        ("projection", options.projection.is_some()),
    ];
    if let Some((name, _)) = columns.iter().find(|(_, set)| *set) {
        polars_bail!(
            ComputeError:
            "`{name}` picks columns out of the file, which a chunk-at-a-time scan cannot honour; \
             use `polars_dyn select` instead"
        )
    }

    if options.column_names_overwrite.is_some() {
        polars_bail!(
            ComputeError:
            "`column_names_overwrite` names the columns of the file, which the chunks take from \
             the schema instead; use `polars_dyn rename`"
        )
    }
    Ok(())
}

struct CsvChunks {
    options: CsvReadOptions,
}

impl ChunkParser for CsvChunks {
    fn schema(&self, chunk: &[u8]) -> PolarsResult<SchemaRef> {
        Ok(self
            .options
            .clone()
            .into_reader_with_file_handle(Cursor::new(chunk))
            .finish()?
            .schema()
            .clone())
    }

    /// The header line is in chunk 0 only, so every later chunk is read without one and takes its
    /// column names from the schema instead.
    fn parse(&self, chunk: &[u8], index: usize, schema: &SchemaRef) -> PolarsResult<DataFrame> {
        self.options
            .clone()
            .with_has_header(self.options.has_header && index == 0)
            .with_schema(Some(schema.clone()))
            .into_reader_with_file_handle(Cursor::new(chunk))
            .finish()
    }
}

struct NdJsonChunks {
    options: NDJsonReadOptions,
}

impl ChunkParser for NdJsonChunks {
    /// The schema polars would settle on for this text: `schema` when it was given, and otherwise
    /// what the chunk infers with `schema_overwrite` laid over it.
    ///
    /// A given `schema` is taken as it is, `schema_overwrite` and all, because that is what
    /// polars' own reader answers with. The laying over is `polars_io`'s own, so that naming a
    /// column the file does not have is the error it is there rather than a line quietly ignored.
    fn schema(&self, chunk: &[u8]) -> PolarsResult<SchemaRef> {
        if let Some(schema) = &self.options.schema {
            return Ok(schema.clone());
        }
        let mut cursor = Cursor::new(chunk);
        let mut schema = ndjson::infer_schema(&mut cursor, self.options.infer_schema_length)?;
        if let Some(overwrite) = &self.options.schema_overwrite {
            overwrite_schema(&mut schema, overwrite)?;
        }
        Ok(SchemaRef::new(schema))
    }

    fn parse(&self, chunk: &[u8], _index: usize, schema: &SchemaRef) -> PolarsResult<DataFrame> {
        ndjson::core::parse_ndjson(chunk, None, schema, self.options.ignore_errors)
    }
}

/// The values `LazyJsonLineReader::new` starts from. `NDJsonReadOptions` has no `Default` and
/// polars offers no builder from it to a plan, so the defaults are repeated here.
fn ndjson_defaults() -> NDJsonReadOptions {
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
///
/// Hive partitioning starts off disabled rather than at polars' "decide from the path": the
/// bytes have no path, and polars refuses to decide for an in-memory buffer.
fn split_opts<T>(defaults: T, opts: &[u8]) -> PolarsResult<(T, UnifiedScanArgs)>
where
    T: Serialize + DeserializeOwned,
{
    let mut record = parse_opts(opts)?;
    let scan = record
        .remove("scan")
        .unwrap_or_else(|| Value::Object(Map::new()));
    let scan_defaults = UnifiedScanArgs {
        hive_options: HiveOptions {
            enabled: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };
    let scan_args = overlay_opts(scan_defaults, &scan)?;
    let options = overlay_opts(defaults, &Value::Object(record))?;
    Ok((options, scan_args))
}
