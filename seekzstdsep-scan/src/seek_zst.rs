//! Reading a seekable zstd file one frame at a time.
//!
//! `seekzstdsep` cuts frames at record boundaries and gives every frame but the last the same
//! record count, so the records of frame `i` are the ones numbered `i * records_per_frame`
//! onwards and a frame decompresses on its own into whole records. This module turns that into a
//! polars scan: it reads frames in parallel, hands each frame's bytes to a [`FrameParser`], and
//! concatenates the results. The parser is whichever polars reader already knows the text format,
//! so no format is parsed here.
//!
//! What a `.seek.zst` source buys over the plain text file is disk: the bytes read shrink with the
//! compression ratio while the file stays appendable and readable record by record. It is not a
//! faster reader. Two pushdowns polars gives its own scans are missing:
//!
//! - A slice with a non-zero offset is not pushed into an anonymous scan at all, so `slice 1000 10`
//!   arrives as no slice: the file is read whole and the plan takes the ten rows from it. Nothing
//!   distinguishes that from an unsliced scan at this boundary, so it cannot even be reported.
//! - `collect --streaming` fails, since polars-stream does not run anonymous scans.
//!
//! `n_rows`, the projection and the predicate all arrive. `n_rows` counts rows of the file, so it
//! is taken first and the other two are applied to what it leaves; without it each frame is
//! narrowed as it is read, so the concatenation never holds rows that were dropped. The dynamic
//! bound that `sort` followed by `slice` produces is the one thing not applied; see
//! `evaluable_part`.
//!
//! The schema is settled from the first frame alone, since the frames are read at once and have to
//! agree on one. Reading the plain file instead lets polars look further, so the two disagree when
//! the first frame does not represent the file: a column that only appears later is missing here,
//! and a column whose type widens later fails to parse rather than widening. An
//! `infer_schema_length` past the first frame reaches no further than it does. Where that matters,
//! pass the schema in `--opts`.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use nu_plugin_polars::scan::ReadAt;
use polars::prelude::{
    AnonymousScan, AnonymousScanArgs, DataFrame, Expr, IntoLazy, LazyFrame, Operator, PolarsError,
    PolarsResult, ScanArgsAnonymous, SchemaRef, polars_bail,
};
use polars_core::runtime::THREAD_POOL;
use polars_core::utils::accumulate_dataframes_vertical;
use rayon::prelude::*;
use seekzstdsep::{AsRead, NoVerify, RecordReader, Verifier, find::Boundary};

/// Records in a `.seek.zst` file are separated by a newline.
const SEPARATOR: &[u8] = b"\n";

/// What a refusal from `seekzstdsep` names the source by; the plugin points at the argument.
const LABEL: &str = "the source";

/// `Read + Seek` over a shared [`ReadAt`], with a position of its own: every thread of a scan
/// holds one over the one handle.
struct ReadAtCursor {
    source: Arc<dyn ReadAt>,
    pos: u64,
}

impl Read for ReadAtCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.source.read_at(self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ReadAtCursor {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let (base, offset) = match pos {
            SeekFrom::Start(n) => (n, 0),
            SeekFrom::End(d) => (self.source.len()?, d),
            SeekFrom::Current(d) => (self.pos, d),
        };
        self.pos = base.checked_add_signed(offset).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a negative or overflowing position",
            )
        })?;
        Ok(self.pos)
    }
}

/// Turns the bytes of one frame into a `DataFrame`.
///
/// A frame holds whole records, so an implementation needs no state carried across frames. It is
/// handed the schema the scan settled on rather than inferring one per frame: independent
/// inference lets two frames disagree about a column's type, and the concatenation then fails.
pub trait FrameParser: Send + Sync {
    /// The schema to read the whole file with, inferred from the first frame.
    fn schema(&self, frame: &[u8]) -> PolarsResult<SchemaRef>;

    /// Reads one frame. `index` is its position in the file, which a format with a header line
    /// uses to read that line in frame 0 only.
    fn parse(&self, frame: &[u8], index: usize, schema: &SchemaRef) -> PolarsResult<DataFrame>;
}

/// A `.seek.zst` source read through `parser`.
///
/// The source is what `seekzstdsep` writes: frames cut at record boundaries, every one but the
/// last holding the same record count. A frame read by record number from a file that breaks that
/// is read from the wrong place, so under [`AsRead`] each frame is checked as it is read, as
/// [`RecordReader::verifying`] describes; what it refuses is a `ComputeError` when the scan is
/// collected. Under [`NoVerify`] nothing is checked and such a file gives wrong rows.
pub struct SeekZstScan<V: Verifier = AsRead> {
    source: Arc<dyn ReadAt>,
    parser: Box<dyn FrameParser>,
    /// Turns a freshly opened reader into the one that checks what `V` asks for.
    verifier: fn(RecordReader<NoVerify, ReadAtCursor>) -> RecordReader<V, ReadAtCursor>,
}

impl SeekZstScan {
    /// Builds the `LazyFrame`, inferring the schema from the first frame and checking the record
    /// count of each frame it reads.
    ///
    /// Reads that one frame now; the rest of the source is left until the frame is collected.
    pub fn lazy_frame(
        source: Arc<dyn ReadAt>,
        parser: Box<dyn FrameParser>,
    ) -> PolarsResult<LazyFrame> {
        Self::lazy_frame_with(source, parser, true)
    }

    /// [`Self::lazy_frame`], checking the record count of each frame only when `verify_frames`.
    ///
    /// The choice is made here once, so the reads of a scan that checks nothing carry none of it.
    pub fn lazy_frame_with(
        source: Arc<dyn ReadAt>,
        parser: Box<dyn FrameParser>,
        verify_frames: bool,
    ) -> PolarsResult<LazyFrame> {
        if verify_frames {
            anonymous_scan(SeekZstScan {
                source,
                parser,
                verifier: RecordReader::verifying,
            })
        } else {
            anonymous_scan(SeekZstScan {
                source,
                parser,
                verifier: std::convert::identity,
            })
        }
    }
}

/// The `LazyFrame` reading `scan`, with the schema inferred from its first frame.
fn anonymous_scan<V: Verifier + 'static>(scan: SeekZstScan<V>) -> PolarsResult<LazyFrame> {
    let schema = scan.infer_schema()?;
    LazyFrame::anonymous_scan(
        Arc::new(scan),
        ScanArgsAnonymous {
            schema: Some(schema),
            ..Default::default()
        },
    )
}

impl<V: Verifier> SeekZstScan<V> {
    fn infer_schema(&self) -> PolarsResult<SchemaRef> {
        let mut reader = self.reader()?;
        let mut buf = Vec::new();
        self.read_frame(&mut reader, &mut buf, 0)?;
        self.parser.schema(&buf)
    }

    /// A reader of its own over the shared source, for the thread that asks.
    fn reader(&self) -> PolarsResult<RecordReader<V, ReadAtCursor>> {
        let cursor = ReadAtCursor {
            source: self.source.clone(),
            pos: 0,
        };
        RecordReader::from_reader(cursor, LABEL, Boundary::Separator(SEPARATOR.to_vec()))
            .map(self.verifier)
            .map_err(|e| error(e.to_string()))
    }

    /// Reads frame `index` into `buf`, which is emptied first.
    ///
    /// The records of the frame are the `records_per_frame` records that start where the frame
    /// does. They are written into the caller's buffer rather than gathered into a fresh one, so
    /// a thread reading many frames allocates once and then reuses the capacity.
    fn read_frame(
        &self,
        reader: &mut RecordReader<V, ReadAtCursor>,
        buf: &mut Vec<u8>,
        index: usize,
    ) -> PolarsResult<()> {
        let per_frame = reader.records_per_frame();
        buf.clear();
        reader
            .records_to(index * per_frame, per_frame, buf)
            .map_err(|e| error(format!("frame {index}: {e}")))
    }
}

fn error(msg: String) -> PolarsError {
    PolarsError::ComputeError(msg.into())
}

impl<V: Verifier + 'static> AnonymousScan for SeekZstScan<V> {
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

    /// Reads the frames and concatenates them, taking `n_rows` of the file and then narrowing
    /// what is left to the rows and columns that were asked for.
    ///
    /// `n_rows` counts the rows of the file, not of the answer, so it is applied before the
    /// predicate: `slice 0 26 | filter ...` asks for the matches among the first 26 rows. How many
    /// rows a frame holds is only known once it is read, so it stops a batch of frames at a time
    /// rather than picking the last frame it needs up front. Without an `n_rows` the whole file is
    /// a single batch, which leaves rayon the most room to balance the work, and each frame is
    /// narrowed as it is read so the concatenation never holds the rows that were dropped.
    fn scan(&self, args: AnonymousScanArgs) -> PolarsResult<DataFrame> {
        let frame_count = self.reader()?.frame_count();
        let (batch_frames, narrow_per_frame) = match args.n_rows {
            Some(_) => (THREAD_POOL.current_num_threads().max(1), false),
            None => (frame_count.max(1), true),
        };

        let mut frames = Vec::new();
        let mut rows = 0;
        for start in (0..frame_count).step_by(batch_frames) {
            let end = start.saturating_add(batch_frames).min(frame_count);
            let parsed = THREAD_POOL.install(|| {
                (start..end)
                    .into_par_iter()
                    .map_init(
                        || self.reader().map(|reader| (reader, Vec::new())),
                        |state, index| {
                            let (reader, buf) = match state {
                                Ok(state) => state,
                                Err(e) => return Err(e.clone()),
                            };
                            self.read_frame(reader, buf, index)?;
                            let df = self.parser.parse(buf, index, &args.schema)?;
                            if narrow_per_frame {
                                narrow(df, &args)
                            } else {
                                Ok(df)
                            }
                        },
                    )
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
/// [`SeekZstScan::scan`] hands it a frame at a time when it can, so that neither the rows nor the
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
