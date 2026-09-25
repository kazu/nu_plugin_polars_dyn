//! Reading a text format from bytes or records in chunks of whole lines.
//!
//! Bytes read by offset are cut into chunks of about `chunk_size`, each cut moved forward to the
//! next newline so a chunk holds whole lines; records come in units of whole records, and each
//! unit is a chunk. The chunks are read in parallel by a [`ChunkParser`] — whichever polars reader
//! already knows the text format — and concatenated. Nothing about where the bytes come from is
//! known here, so a local file, a remote object and the decompressed records of either are read
//! the same way.
//!
//! A cut in bytes is made at any newline: nothing here knows the format's quoting, so a newline
//! inside a quoted csv field is a cut like any other when it is the first one past a `chunk_size`
//! boundary, and the two halves are then parsed as records, which the parser refuses or misreads.
//! A source whose records hold newlines is read whole with a `chunk_size` at least its length.
//! Records are taken as they end, so a record source that does not end them with a newline gives
//! the parser lines it does not know.
//!
//! Nothing but the first chunk is read before the frame is collected. What the query asks for
//! arrives then, and two pushdowns polars gives its own scans are missing, since this is an
//! `AnonymousScan`:
//!
//! - A slice with a non-zero offset is not pushed into an anonymous scan at all, so `slice 1000 10`
//!   arrives as no slice: the bytes are read whole and the plan takes the ten rows from it. Nothing
//!   distinguishes that from an unsliced scan at this boundary, so it cannot even be reported.
//! - `collect --streaming` fails, since polars-stream does not run anonymous scans.
//!
//! `n_rows`, the projection and the predicate all arrive. `n_rows` counts rows of the source, so it
//! is taken first and the other two are applied to what it leaves; it also bounds what is asked of
//! the source, which is cut, or read a unit at a time, only as far as the rows run. Without it each
//! chunk is narrowed as it is read, so the concatenation never holds rows that were dropped. The
//! dynamic bound that `sort` followed by `slice` produces is the one thing not applied; see
//! `evaluable_part`.
//!
//! The schema is settled from the first chunk alone, since the chunks are read at once and have to
//! agree on one. A column that only appears later is missing, and a column whose type widens later
//! fails to parse rather than widening. An `infer_schema_length` past the first chunk reaches no
//! further than it does. Where that matters, pass the schema in `--opts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use polars::prelude::{
    AnonymousScan, AnonymousScanArgs, DataFrame, Expr, IntoLazy, LazyFrame, Operator, PolarsError,
    PolarsResult, ScanArgsAnonymous, SchemaRef, polars_bail,
};
use polars_core::runtime::THREAD_POOL;
use polars_core::utils::accumulate_dataframes_vertical;
use rayon::prelude::*;

use super::read_at::{ReadAt, read_fully};
use super::{Bytes, Records};

/// How many bytes a cut looks ahead for a newline at a time.
const PROBE: usize = 8 * 1024;

/// Turns the bytes of one chunk into a `DataFrame`.
///
/// A chunk holds whole lines, so an implementation needs no state carried across chunks. It is
/// handed the schema the scan settled on rather than inferring one per chunk: independent
/// inference lets two chunks disagree about a column's type, and the concatenation then fails.
pub trait ChunkParser: Send + Sync {
    /// The schema to read the whole source with, inferred from the first chunk.
    fn schema(&self, chunk: &[u8]) -> PolarsResult<SchemaRef>;

    /// Reads one chunk. `index` is its position among the chunks, counted from the one the schema
    /// was inferred from, which a format with a header line uses to read that line in chunk 0 only.
    fn parse(&self, chunk: &[u8], index: usize, schema: &SchemaRef) -> PolarsResult<DataFrame>;
}

/// A source read through `parser` in chunks of whole lines.
pub struct ChunkedScan {
    source: Bytes,
    parser: Box<dyn ChunkParser>,
    chunk_size: usize,
    /// The unit of records the first chunk is: the first one holding a record, which is where a
    /// header line is. Always 0 for bytes.
    first_unit: usize,
}

impl ChunkedScan {
    /// Builds the `LazyFrame` over chunks of about `chunk_size` bytes, inferring the schema from
    /// the first.
    ///
    /// Reads that one chunk now; the rest is left until the frame is collected.
    pub fn lazy_frame(
        source: Bytes,
        parser: Box<dyn ChunkParser>,
        chunk_size: usize,
    ) -> PolarsResult<LazyFrame> {
        let scan = ChunkedScan {
            source,
            parser,
            chunk_size: chunk_size.max(1),
            first_unit: 0,
        };
        let (schema, first_unit) = scan.infer_schema()?;
        let scan = ChunkedScan { first_unit, ..scan };
        LazyFrame::anonymous_scan(
            Arc::new(scan),
            ScanArgsAnonymous {
                schema: Some(schema),
                ..Default::default()
            },
        )
    }

    /// The schema inferred from the first chunk, and for records the unit that chunk is.
    fn infer_schema(&self) -> PolarsResult<(SchemaRef, usize)> {
        let mut buf = Vec::new();
        let mut first = 0;
        match &self.source {
            Bytes::At(source) => {
                let len = source.len().map_err(|e| error(e.to_string()))?;
                let end = next_cut(&**source, 0, self.chunk_size, len)?;
                read_chunk(&**source, &mut buf, 0, (0, end))?;
            }
            Bytes::Records(records) => {
                while records.read_unit(first, &mut buf)? && buf.is_empty() {
                    first += 1;
                }
            }
        }
        Ok((self.parser.schema(&buf)?, first))
    }

    /// The frames of the chunks of `source` up to `n_rows`: all of them, cut up front and read
    /// at once, without one, and else a batch at a time, cut only as far as the batch reaches.
    fn frames_at(
        &self,
        source: &dyn ReadAt,
        args: &AnonymousScanArgs,
    ) -> PolarsResult<Vec<DataFrame>> {
        let Some(n_rows) = args.n_rows else {
            let chunks = cut_chunks(source, self.chunk_size)?;
            return self.parse_each(
                chunks.into_iter().enumerate().collect(),
                args,
                |buf, i, r| {
                    read_chunk(source, buf, i, r)?;
                    self.parser.parse(buf, i, &args.schema)
                },
            );
        };

        let len = source.len().map_err(|e| error(e.to_string()))?;
        let mut frames = Vec::new();
        let (mut rows, mut start, mut index) = (0, 0, 0);
        while start < len || index == 0 {
            let mut batch = Vec::new();
            while batch.len() < batch_chunks() && (start < len || index == 0) {
                let end = next_cut(source, start, self.chunk_size, len)?;
                batch.push((index, (start, end)));
                (start, index) = (end, index + 1);
            }
            let parsed = self.parse_each(batch, args, |buf, i, r| {
                read_chunk(source, buf, i, r)?;
                self.parser.parse(buf, i, &args.schema)
            })?;
            rows += parsed.iter().map(DataFrame::height).sum::<usize>();
            frames.extend(parsed);
            if rows >= n_rows {
                break;
            }
        }
        Ok(frames)
    }

    /// The frames of the units of `records`, as many at a time as there are threads, stopping once
    /// the rows in the order of the units reach `n_rows`.
    ///
    /// Without an `n_rows` all the units are read at once when `records` knows how many there are.
    /// With one, the first read takes the units `records` says the first `n_rows` records are in,
    /// and the reads after it take a batch at a time: a record need not be a row — the header line
    /// of a csv is not, nor is a blank line — so those units may still fall short. A unit holding
    /// no record is an empty frame, and one past the last ends the reads.
    fn frames_of_records(
        &self,
        records: &dyn Records,
        args: &AnonymousScanArgs,
    ) -> PolarsResult<Vec<DataFrame>> {
        let count = records.count_units();
        let first = match args.n_rows {
            Some(n) => records
                .count_units_for(n)
                .unwrap_or_else(batch_chunks)
                .max(1),
            None => count.unwrap_or_else(batch_chunks),
        };
        let past_end = AtomicBool::new(false);
        let empty = || DataFrame::empty_with_schema(&args.schema);
        let mut frames = Vec::new();
        let (mut rows, mut next) = (0, 0);
        loop {
            let wanted = if next == 0 { first } else { batch_chunks() };
            let end = count.map_or(next + wanted, |count| (next + wanted).min(count));
            if end <= next {
                break;
            }
            let parsed = self.parse_each(
                (next..end).map(|i| (i, ())).collect(),
                args,
                |buf, i, ()| {
                    buf.clear();
                    if !records.read_unit(i, buf)? {
                        past_end.store(true, Ordering::Relaxed);
                        return Ok(empty());
                    }
                    if buf.is_empty() {
                        return Ok(empty());
                    }
                    self.parser
                        .parse(buf, i.saturating_sub(self.first_unit), &args.schema)
                },
            )?;
            rows += parsed.iter().map(DataFrame::height).sum::<usize>();
            frames.extend(parsed);
            next = end;
            if past_end.load(Ordering::Relaxed) || args.n_rows.is_some_and(|n| rows >= n) {
                break;
            }
        }
        if frames.is_empty() {
            frames.push(match args.n_rows {
                Some(_) => empty(),
                None => narrow(empty(), args)?,
            });
        }
        Ok(frames)
    }

    /// Parses `chunks` in parallel through `parse`, which is handed a buffer of its thread's to
    /// read into, and narrows each result unless an `n_rows` has to count its rows first.
    fn parse_each<T: Send>(
        &self,
        chunks: Vec<(usize, T)>,
        args: &AnonymousScanArgs,
        parse: impl Fn(&mut Vec<u8>, usize, T) -> PolarsResult<DataFrame> + Sync,
    ) -> PolarsResult<Vec<DataFrame>> {
        THREAD_POOL.install(|| {
            chunks
                .into_par_iter()
                .map_init(Vec::new, |buf, (index, chunk)| {
                    let df = parse(buf, index, chunk)?;
                    match args.n_rows {
                        Some(_) => Ok(df),
                        None => narrow(df, args),
                    }
                })
                .collect()
        })
    }
}

/// How many chunks are read at once while `n_rows` is being counted: one per thread.
fn batch_chunks() -> usize {
    THREAD_POOL.current_num_threads().max(1)
}

/// Reads the chunk at `range` into `buf`, which is emptied first, so a thread reading many chunks
/// allocates once and then reuses the capacity.
fn read_chunk(
    source: &dyn ReadAt,
    buf: &mut Vec<u8>,
    index: usize,
    (start, end): (u64, u64),
) -> PolarsResult<()> {
    let len = usize::try_from(end - start).map_err(|e| error(e.to_string()))?;
    buf.clear();
    buf.resize(len, 0);
    let read = read_fully(source, start, buf).map_err(|e| error(e.to_string()))?;
    if read != len {
        polars_bail!(
            ComputeError:
            "chunk {index}: the source ended after {read} of {len} bytes"
        )
    }
    Ok(())
}

/// The end of the chunk that starts at `start`: just after the first newline at or after
/// `start + chunk_size`, or `len` when there is none or that is past the end.
fn next_cut(source: &dyn ReadAt, start: u64, chunk_size: usize, len: u64) -> PolarsResult<u64> {
    let target = start.saturating_add(chunk_size as u64);
    if target >= len {
        return Ok(len);
    }
    Ok(next_line_start(source, target, len)?.unwrap_or(len))
}

/// The byte ranges of the chunks of `source`: cuts every `chunk_size` bytes, each moved forward
/// to just after the next newline. A source without a newline, or shorter than `chunk_size`, is
/// one chunk; an empty source is one empty chunk. A newline at the very end never opens an empty
/// chunk after it.
fn cut_chunks(source: &dyn ReadAt, chunk_size: usize) -> PolarsResult<Vec<(u64, u64)>> {
    let len = source.len().map_err(|e| error(e.to_string()))?;
    let chunk_size = chunk_size as u64;
    let targets: Vec<u64> = (1..)
        .map(|k| k * chunk_size)
        .take_while(|&target| target < len)
        .collect();
    let cuts = THREAD_POOL.install(|| {
        targets
            .into_par_iter()
            .map(|target| next_line_start(source, target, len))
            .collect::<PolarsResult<Vec<_>>>()
    })?;

    let mut chunks = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for cut in cuts.into_iter().flatten() {
        if cut > start && cut < len {
            chunks.push((start, cut));
            start = cut;
        }
    }
    chunks.push((start, len));
    Ok(chunks)
}

/// The offset just after the first newline at or after `from`, or `None` when there is none
/// before `len`.
fn next_line_start(source: &dyn ReadAt, from: u64, len: u64) -> PolarsResult<Option<u64>> {
    let mut buf = [0u8; PROBE];
    let mut at = from;
    while at < len {
        let read = read_fully(source, at, &mut buf).map_err(|e| error(e.to_string()))?;
        if read == 0 {
            break;
        }
        if let Some(i) = buf[..read].iter().position(|&b| b == b'\n') {
            return Ok(Some(at + i as u64 + 1));
        }
        at += read as u64;
    }
    Ok(None)
}

fn error(msg: String) -> PolarsError {
    PolarsError::ComputeError(msg.into())
}

impl AnonymousScan for ChunkedScan {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn schema(&self, _infer_schema_length: Option<usize>) -> PolarsResult<SchemaRef> {
        self.infer_schema().map(|(schema, _)| schema)
    }

    fn allows_projection_pushdown(&self) -> bool {
        true
    }

    /// Enabled, which makes polars hand over the predicate instead of filtering the result, so
    /// [`Self::scan`] has to apply it itself. The one it leaves to the plan is the dynamic bound a
    /// `sort` before a `slice` produces, which `evaluable_part` takes out.
    fn allows_predicate_pushdown(&self) -> bool {
        true
    }

    /// Reads the chunks and concatenates them, taking `n_rows` of the source and then narrowing
    /// what is left to the rows and columns that were asked for.
    ///
    /// `n_rows` counts the rows of the source, not of the answer, so it is applied before the
    /// predicate: `slice 0 26 | filter ...` asks for the matches among the first 26 rows. How many
    /// rows a chunk holds is only known once it is read, so it stops a batch of chunks at a time
    /// rather than picking the last chunk it needs up front. Without an `n_rows` each chunk is
    /// narrowed as it is read so the concatenation never holds the rows that were dropped.
    fn scan(&self, args: AnonymousScanArgs) -> PolarsResult<DataFrame> {
        let frames = match &self.source {
            Bytes::At(source) => self.frames_at(&**source, &args)?,
            Bytes::Records(records) => self.frames_of_records(&**records, &args)?,
        };
        let df = accumulate_dataframes_vertical(frames)?;
        match args.n_rows {
            Some(n) => narrow(df.head(Some(n)), &args),
            None => Ok(df),
        }
    }
}

/// Applies the pushed-down predicate and projection to what it is handed.
///
/// [`ChunkedScan::scan`] hands it a chunk at a time when it can, so that neither the rows nor the
/// columns that were not asked for are carried to the concatenation. With an `n_rows` it is the
/// concatenation instead, since those rows are counted before the predicate takes any away.
fn narrow(df: DataFrame, args: &AnonymousScanArgs) -> PolarsResult<DataFrame> {
    let mut lazy = df.lazy();
    if let Some(predicate) = args.predicate.as_ref()
        && let Some(evaluable) = evaluable_part(predicate)?
    {
        lazy = lazy.filter(evaluable);
    }
    if let Some(columns) = &args.with_columns {
        lazy = lazy.select(
            columns
                .iter()
                .map(|name| polars::prelude::col(name.clone()))
                .collect::<Vec<_>>(),
        );
    }
    lazy.collect()
}

/// The part of `predicate` this scan can evaluate, which is all of it but the bound that `sort`
/// followed by `slice` produces.
///
/// That bound is a dynamic top-k threshold, a reference to state the optimizer updates as the scan
/// runs. It has no form in the language `LazyFrame::filter` takes, so polars hands it over as
/// `Expr::Display`, a placeholder that exists to be printed; lowering it back aborts the process
/// (`Should never go from IR -> DSL -> IR`).
///
/// Leaving it unapplied returns rows the bound would have dropped, which is sound because it is an
/// accelerator rather than part of the answer: the `Sort` that produced it keeps its own slice and
/// takes the top rows from whatever the scan returns. polars relies on the same thing, evaluating
/// the bound as all-true until it is set. Every other conjunct has to be applied, since a `Filter`
/// that is pushed down is taken out of the plan and nothing else will apply it.
///
/// Whatever else polars pushes down arrives as its own conjunct, so the two are separated by
/// splitting on `and` rather than judging the predicate as a whole. An `Expr::Display` anywhere
/// but as a conjunct of its own is a shape this does not know how to take apart, and is an error
/// rather than a guess about which rows to keep.
fn evaluable_part(predicate: &Expr) -> PolarsResult<Option<Expr>> {
    let mut keep: Option<Expr> = None;
    for conjunct in conjuncts(predicate) {
        if matches!(conjunct, Expr::Display { .. }) {
            continue;
        }
        if conjunct
            .into_iter()
            .any(|e| matches!(e, Expr::Display { .. }))
        {
            polars_bail!(
                ComputeError:
                "the pushed-down predicate holds a bound this scan cannot evaluate: {conjunct}"
            )
        }
        keep = Some(match keep {
            Some(kept) => kept.and(conjunct.clone()),
            None => conjunct.clone(),
        });
    }
    Ok(keep)
}

/// The expressions `predicate` is the `and` of, which is `predicate` itself when it is not one.
fn conjuncts(predicate: &Expr) -> Vec<&Expr> {
    match predicate {
        Expr::BinaryExpr {
            left,
            op: Operator::And,
            right,
        } => {
            let mut out = conjuncts(left);
            out.extend(conjuncts(right));
            out
        }
        other => vec![other],
    }
}
