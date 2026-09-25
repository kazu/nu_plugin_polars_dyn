//! A seekable zstd file as the records it decompresses to, a frame at a time.
//!
//! [`SeekZst`] wraps the bytes it is handed — the compressed file, wherever it came from — into
//! [`Records`] whose units are the frames. A unit is read through seekzstdsep's [`RecordReader`]:
//! the frame is decompressed on its own and its records are cut out of it by seekzstdsep's finder,
//! the one place that knows where a record ends, borrowed from the read window and written on
//! only when `record_filter` keeps them. The frames are independent, so the scan after it reads as
//! many at once as it has threads, and a frame nobody asks for is neither decompressed nor read
//! from the source.

use std::sync::{Arc, Mutex};

use memchr::memmem::Finder;
use nu_plugin_polars::scan::{Bytes, ReadAt, ReadAtCursor, Records, ScanSource, parse_opts};
use polars::prelude::{PolarsError, PolarsResult, polars_bail};
use seekzstdsep::{NoVerify, RecordReader, Verifier, find};
use serde::Deserialize;
use serde_json::Value;

/// What a refusal from `seekzstdsep` names the source by; the plugin points at the argument.
const LABEL: &str = "the source";

/// The `seek-zst` source: suffix `.seek.zst`, wrap only.
pub struct SeekZst;

impl ScanSource for SeekZst {
    fn name(&self) -> &'static str {
        "seek-zst"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".seek.zst"]
    }

    /// Opens `source` once to check that it is a seekable zstd file whose frame 0 holds a record,
    /// and hands on its frames.
    fn wrap(&self, source: Bytes, opts: &[u8]) -> PolarsResult<Bytes> {
        let Bytes::At(source) = source else {
            polars_bail!(
                ComputeError:
                "`seek-zst` reads its source by offset, and the one before it hands records"
            )
        };
        let opts: SeekZstOpts = serde_json::from_value(Value::Object(parse_opts(opts)?))
            .map_err(|e| error(format!("opts: {e}")))?;
        let filter = opts
            .record_filter
            .as_deref()
            .map(|needle| Finder::new(needle.as_bytes()).into_owned());
        let records: Arc<dyn Records> = if opts.verify_frames {
            Arc::new(Frames::open(source, opts, filter, RecordReader::verifying)?)
        } else {
            Arc::new(Frames::open(source, opts, filter, std::convert::identity)?)
        };
        Ok(Bytes::Records(records))
    }
}

/// `--opts {seek-zst: {...}}`. Every field may be left out; an unknown key is an error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct SeekZstOpts {
    /// Where a record ends, as seekzstdsep's `--finder` names it: `sep`, `fixed`, `flatbuffers`
    /// or `msgpack`.
    finder: String,
    /// seekzstdsep's `--finder-arg`: the separator of `sep`, `"\n"` when left out, and the
    /// length of `fixed`.
    finder_arg: Option<String>,
    /// Whether every frame is checked to hold the record count seekzstdsep writes, as
    /// [`RecordReader::verifying`] describes.
    verify_frames: bool,
    /// Keeps only the records holding these bytes, before any parser sees them.
    record_filter: Option<String>,
}

impl Default for SeekZstOpts {
    fn default() -> Self {
        Self {
            finder: "sep".into(),
            finder_arg: None,
            verify_frames: true,
            record_filter: None,
        }
    }
}

/// The frames of one `.seek.zst` source, each a unit of [`Records`].
///
/// A unit is found by record number — frame `k` holds the records from `k * records_per_frame` —
/// so a file whose frames hold uneven record counts is read from the wrong place unless `V`
/// checks them. Opening a reader reads the seek table and frame 0, so the readers are kept in a
/// pool and each thread takes one, or opens one when the pool is empty.
struct Frames<V: Verifier> {
    source: Arc<dyn ReadAt>,
    opts: SeekZstOpts,
    filter: Option<Finder<'static>>,
    /// Turns a freshly opened reader into the one that checks what `V` asks for.
    verifier: fn(RecordReader<NoVerify, ReadAtCursor>) -> RecordReader<V, ReadAtCursor>,
    frame_count: usize,
    records_per_frame: usize,
    pool: Mutex<Vec<RecordReader<V, ReadAtCursor>>>,
}

impl<V: Verifier> Frames<V> {
    /// Opens `source` once, for the frame count and the records per frame, and keeps that reader.
    fn open(
        source: Arc<dyn ReadAt>,
        opts: SeekZstOpts,
        filter: Option<Finder<'static>>,
        verifier: fn(RecordReader<NoVerify, ReadAtCursor>) -> RecordReader<V, ReadAtCursor>,
    ) -> PolarsResult<Self> {
        let mut frames = Frames {
            source,
            opts,
            filter,
            verifier,
            frame_count: 0,
            records_per_frame: 0,
            pool: Mutex::new(Vec::new()),
        };
        let reader = frames.open_reader()?;
        frames.frame_count = reader.frame_count();
        frames.records_per_frame = reader.records_per_frame();
        frames.put_back(reader);
        Ok(frames)
    }

    fn open_reader(&self) -> PolarsResult<RecordReader<V, ReadAtCursor>> {
        let boundary = find::from_spec(&self.opts.finder, self.opts.finder_arg.as_deref())
            .map_err(|e| error(format!("opts: {e}")))?;
        RecordReader::from_reader(ReadAtCursor::new(self.source.clone()), LABEL, boundary)
            .map(self.verifier)
            .map_err(|e| error(e.to_string()))
    }

    /// A reader from the pool, or a new one.
    fn reader(&self) -> PolarsResult<RecordReader<V, ReadAtCursor>> {
        let pooled = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop();
        match pooled {
            Some(reader) => Ok(reader),
            None => self.open_reader(),
        }
    }

    /// Not called for a reader whose read failed: its state is unknown, so it is dropped.
    fn put_back(&self, reader: RecordReader<V, ReadAtCursor>) {
        self.pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(reader);
    }
}

impl<V: Verifier + Send> Records for Frames<V>
where
    RecordReader<V, ReadAtCursor>: Send,
{
    /// Frame `index`: its records, those without `record_filter` dropped, copied from the read
    /// window only when kept.
    fn read_unit(&self, index: usize, dst: &mut Vec<u8>) -> PolarsResult<bool> {
        if index >= self.frame_count {
            return Ok(false);
        }
        let mut reader = self.reader()?;
        let from = index * self.records_per_frame;
        let cnt = if index + 1 == self.frame_count {
            usize::MAX
        } else {
            self.records_per_frame
        };
        reader
            .fold_records(from, cnt, (), |(), record| {
                if self
                    .filter
                    .as_ref()
                    .is_none_or(|f| f.find(record).is_some())
                {
                    dst.extend_from_slice(record);
                }
                Ok(())
            })
            .map_err(|e| error(format!("frame {index}: {e}")))?;
        self.put_back(reader);
        Ok(true)
    }

    fn count_units(&self) -> Option<usize> {
        Some(self.frame_count)
    }

    /// The frames holding the first `n_records` records, when every record is kept; with a
    /// `record_filter` how many are kept is only known by reading them.
    fn count_units_for(&self, n_records: usize) -> Option<usize> {
        if self.filter.is_some() {
            return None;
        }
        Some(
            n_records
                .div_ceil(self.records_per_frame)
                .min(self.frame_count),
        )
    }
}

fn error(msg: String) -> PolarsError {
    PolarsError::ComputeError(msg.into())
}
