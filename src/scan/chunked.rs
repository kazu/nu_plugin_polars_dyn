//! Reading a text format from bytes in newline-cut chunks.
//!
//! The bytes are cut into chunks of about `chunk_size`, each cut moved forward to the next
//! newline so a chunk holds whole lines, and the chunks are read in parallel by a
//! [`ChunkParser`] — whichever polars reader already knows the text format — and concatenated.
//! The bytes are a [`ReadAt`], so this reads a local file, a remote object and the decompressed
//! form of either the same way; nothing about where the chunks come from is known here, which is
//! also why a chunk that straddles two frames of a compressed source is read as it is.
//!
//! A cut is made at any newline: nothing here knows the format's quoting, so a newline inside a
//! quoted csv field is a cut like any other when it is the first one past a `chunk_size` boundary,
//! and the two halves are then parsed as records, which the parser refuses or misreads. A source
//! whose records hold newlines is read whole with a `chunk_size` at least its length.
//!
//! Two pushdowns polars gives its own scans are missing, since this is an `AnonymousScan`:
//!
//! - A slice with a non-zero offset is not pushed into an anonymous scan at all, so `slice 1000 10`
//!   arrives as no slice: the bytes are read whole and the plan takes the ten rows from it. Nothing
//!   distinguishes that from an unsliced scan at this boundary, so it cannot even be reported.
//! - `collect --streaming` fails, since polars-stream does not run anonymous scans.
//!
//! `n_rows`, the projection and the predicate all arrive. `n_rows` counts rows of the source, so it
//! is taken first and the other two are applied to what it leaves; without it each chunk is
//! narrowed as it is read, so the concatenation never holds rows that were dropped. The dynamic
//! bound that `sort` followed by `slice` produces is the one thing not applied; see
//! `evaluable_part`.
//!
//! The schema is settled from the first chunk alone, since the chunks are read at once and have to
//! agree on one. A column that only appears later is missing, and a column whose type widens later
//! fails to parse rather than widening. An `infer_schema_length` past the first chunk reaches no
//! further than it does. Where that matters, pass the schema in `--opts`.

use std::sync::Arc;

use polars::prelude::{
    AnonymousScan, AnonymousScanArgs, DataFrame, Expr, IntoLazy, LazyFrame, Operator, PolarsError,
    PolarsResult, ScanArgsAnonymous, SchemaRef, polars_bail,
};
use polars_core::runtime::THREAD_POOL;
use polars_core::utils::accumulate_dataframes_vertical;
use rayon::prelude::*;

use super::read_at::{ReadAt, read_fully};

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

    /// Reads one chunk. `index` is its position in the source, which a format with a header line
    /// uses to read that line in chunk 0 only.
    fn parse(&self, chunk: &[u8], index: usize, schema: &SchemaRef) -> PolarsResult<DataFrame>;
}

/// A source read through `parser` in chunks cut at newlines.
pub struct ChunkedScan {
    source: Arc<dyn ReadAt>,
    parser: Box<dyn ChunkParser>,
    /// The byte ranges of the chunks, each starting at 0 or just after a newline and ending at
    /// the source's end or just after a newline.
    chunks: Vec<(u64, u64)>,
}

impl ChunkedScan {
    /// Builds the `LazyFrame`, cutting `source` into chunks of about `chunk_size` bytes and
    /// inferring the schema from the first.
    ///
    /// Reads that one chunk now, and probes the source for the cuts; the rest is left until the
    /// frame is collected.
    pub fn lazy_frame(
        source: Arc<dyn ReadAt>,
        parser: Box<dyn ChunkParser>,
        chunk_size: usize,
    ) -> PolarsResult<LazyFrame> {
        let chunks = cut_chunks(&*source, chunk_size)?;
        let scan = ChunkedScan {
            source,
            parser,
            chunks,
        };
        let schema = scan.infer_schema()?;
        LazyFrame::anonymous_scan(
            Arc::new(scan),
            ScanArgsAnonymous {
                schema: Some(schema),
                ..Default::default()
            },
        )
    }

    fn infer_schema(&self) -> PolarsResult<SchemaRef> {
        let mut buf = Vec::new();
        self.read_chunk(&mut buf, 0)?;
        self.parser.schema(&buf)
    }

    /// Reads chunk `index` into `buf`, which is emptied first, so a thread reading many chunks
    /// allocates once and then reuses the capacity.
    fn read_chunk(&self, buf: &mut Vec<u8>, index: usize) -> PolarsResult<()> {
        let (start, end) = self.chunks[index];
        let len = usize::try_from(end - start).map_err(|e| error(e.to_string()))?;
        buf.clear();
        buf.resize(len, 0);
        let read = read_fully(&*self.source, start, buf).map_err(|e| error(e.to_string()))?;
        if read != len {
            polars_bail!(
                ComputeError:
                "chunk {index}: the source ended after {read} of {len} bytes"
            )
        }
        Ok(())
    }
}

/// The byte ranges of the chunks of `source`: cuts every `chunk_size` bytes, each moved forward
/// to just after the next newline. A source without a newline, or shorter than `chunk_size`, is
/// one chunk; an empty source is one empty chunk. A newline at the very end never opens an empty
/// chunk after it.
fn cut_chunks(source: &dyn ReadAt, chunk_size: usize) -> PolarsResult<Vec<(u64, u64)>> {
    let len = source.len().map_err(|e| error(e.to_string()))?;
    let chunk_size = chunk_size.max(1) as u64;
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
        self.infer_schema()
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
    /// rather than picking the last chunk it needs up front. Without an `n_rows` the whole source
    /// is a single batch, which leaves rayon the most room to balance the work, and each chunk is
    /// narrowed as it is read so the concatenation never holds the rows that were dropped.
    fn scan(&self, args: AnonymousScanArgs) -> PolarsResult<DataFrame> {
        let chunk_count = self.chunks.len();
        let (batch_chunks, narrow_per_chunk) = match args.n_rows {
            Some(_) => (THREAD_POOL.current_num_threads().max(1), false),
            None => (chunk_count, true),
        };

        let mut frames = Vec::new();
        let mut rows = 0;
        for start in (0..chunk_count).step_by(batch_chunks) {
            let end = start.saturating_add(batch_chunks).min(chunk_count);
            let parsed = THREAD_POOL.install(|| {
                (start..end)
                    .into_par_iter()
                    .map_init(Vec::new, |buf, index| {
                        self.read_chunk(buf, index)?;
                        let df = self.parser.parse(buf, index, &args.schema)?;
                        if narrow_per_chunk {
                            narrow(df, &args)
                        } else {
                            Ok(df)
                        }
                    })
                    .collect::<PolarsResult<Vec<_>>>()
            })?;
            rows += parsed.iter().map(DataFrame::height).sum::<usize>();
            frames.extend(parsed);
            if args.n_rows.is_some_and(|n| rows >= n) {
                break;
            }
        }

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
