use std::{fs::File, io};

/// Bytes read by offset, from a handle shared between threads.
///
/// Every read names its offset and takes `&self`, so one handle serves the frames a scan reads in
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
