use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    sync::Arc,
};

/// Bytes read by offset, from a handle shared between threads.
///
/// Every read names its offset and takes `&self`, so one handle serves the chunks a scan reads in
/// parallel without a position to share or a handle to clone; a remote store's range request has
/// the same shape. A source that cannot be read by offset — a pipe, a socket — has no
/// implementation.
#[expect(
    clippy::len_without_is_empty,
    reason = "`len` is a request to the store; nothing asks whether it is empty"
)]
pub trait ReadAt: Send + Sync {
    /// Reads from `offset` into `buf` and returns how many bytes were read. Fewer than `buf.len()`
    /// can come back before the end; `0` means `offset` is at or past the end.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// The length in bytes.
    fn len(&self) -> io::Result<u64>;
}

#[cfg(unix)]
impl ReadAt for File {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

#[cfg(windows)]
impl ReadAt for File {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }
}

/// Fills `buf` from `offset`, stopping early only at the end of the source.
///
/// [`ReadAt::read_at`] may return fewer bytes than asked for; this keeps asking. Returns how many
/// bytes were read, which is `buf.len()` unless the source ended first.
pub fn read_fully(source: &dyn ReadAt, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = source.read_at(offset + filled as u64, &mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// `Read + Seek` over a shared [`ReadAt`], with a position of its own.
///
/// For a library that wants a reader rather than offsets: every thread holds one over the one
/// handle. It is what the `.seek.zst` scan hands zeekstd and what logfmt-scan hands polars-logfmt.
///
/// ```
/// # fn main() -> std::io::Result<()> {
/// use std::io::{Read, Seek, SeekFrom};
/// use std::sync::Arc;
/// use nu_plugin_polars::scan::{ReadAt, ReadAtCursor};
/// # let dir = tempfile::tempdir()?;
/// # let path = dir.path().join("x");
/// # std::fs::write(&path, b"hello")?;
/// let source: Arc<dyn ReadAt> = Arc::new(std::fs::File::open(path)?);
/// let mut cursor = ReadAtCursor::new(source);
/// cursor.seek(SeekFrom::Start(1))?;
/// let mut rest = String::new();
/// cursor.read_to_string(&mut rest)?;
/// assert_eq!(rest, "ello");
/// # Ok(())
/// # }
/// ```
pub struct ReadAtCursor {
    source: Arc<dyn ReadAt>,
    pos: u64,
}

impl ReadAtCursor {
    /// A cursor at offset 0.
    pub fn new(source: Arc<dyn ReadAt>) -> Self {
        Self { source, pos: 0 }
    }
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
