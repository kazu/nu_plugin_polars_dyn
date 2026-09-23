//! A seekable zstd file as the bytes it decompresses to.
//!
//! [`SeekZst`] wraps the bytes it is handed — the compressed file, wherever it came from — into a
//! [`ReadAt`] over the decompressed stream. A read at an offset finds the frame holding it in the
//! seek table, decompresses that frame from its start up to the offset and hands out what
//! follows; a read that runs past the frame continues into the next. So a reader that asks for
//! chunks of many frames decompresses each frame once, and one that probes a few bytes here and
//! there decompresses a frame per probe. Nothing here depends on how the frames were cut.
//!
//! zeekstd's decoder holds state that cannot be shared between threads, so a pool of decoders is
//! kept: a read takes one, or makes one when the pool is empty, and puts it back. Every decoder
//! reads the compressed bytes through a cursor of its own over the one shared handle.

use std::io;
use std::sync::{Arc, Mutex};

use nu_plugin_polars::scan::{ReadAt, ReadAtCursor, ScanSource, no_opts};
use polars::prelude::{PolarsError, PolarsResult};
use zeekstd::{DecodeOptions, Decoder, SeekTable};

/// The `seek-zst` source: suffix `.seek.zst`, wrap only, no options.
pub struct SeekZst;

impl ScanSource for SeekZst {
    fn name(&self) -> &'static str {
        "seek-zst"
    }

    fn suffixes(&self) -> &'static [&'static str] {
        &[".seek.zst"]
    }

    fn wrap(&self, source: Arc<dyn ReadAt>, opts: &[u8]) -> PolarsResult<Arc<dyn ReadAt>> {
        no_opts(self.name(), opts)?;
        Decompressed::new(source)
            .map(|d| Arc::new(d) as Arc<dyn ReadAt>)
            .map_err(|e| PolarsError::ComputeError(e.to_string().into()))
    }
}

/// The decompressed bytes of a seekable zstd source.
struct Decompressed {
    source: Arc<dyn ReadAt>,
    seek_table: SeekTable,
    len: u64,
    pool: Mutex<Vec<Decoder<'static, ReadAtCursor>>>,
}

impl Decompressed {
    /// Reads the seek table of `source` now; the frames are read as they are asked for.
    fn new(source: Arc<dyn ReadAt>) -> zeekstd::Result<Self> {
        let seek_table = SeekTable::from_seekable(&mut ReadAtCursor::new(source.clone()))?;
        let len = seek_table.size_decomp();
        Ok(Self {
            source,
            seek_table,
            len,
            pool: Mutex::new(Vec::new()),
        })
    }

    /// A decoder from the pool, or a new one over the seek table already read.
    fn decoder(&self) -> zeekstd::Result<Decoder<'static, ReadAtCursor>> {
        let pooled = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop();
        match pooled {
            Some(decoder) => Ok(decoder),
            None => DecodeOptions::new(ReadAtCursor::new(self.source.clone()))
                .seek_table(self.seek_table.clone())
                .into_decoder(),
        }
    }

    /// Not called for a decoder whose read failed: its state is unknown, so it is dropped.
    fn put_back(&self, decoder: Decoder<'static, ReadAtCursor>) {
        self.pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(decoder);
    }
}

impl ReadAt for Decompressed {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let mut decoder = self.decoder().map_err(io::Error::other)?;
        decoder.set_offset(offset).map_err(io::Error::other)?;
        let mut filled = 0;
        while filled < buf.len() {
            let n = decoder
                .decompress(&mut buf[filled..])
                .map_err(io::Error::other)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        self.put_back(decoder);
        Ok(filled)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.len)
    }
}
