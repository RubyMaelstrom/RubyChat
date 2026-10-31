//! Files mapped read-only into memory: fonts, of which only the pages
//! actually read become resident, so a 100 MB collection costs what its
//! glyphs in use do.

use std::fs::File;
use std::io;
use std::path::Path;

/// A file mapped for reading. Dropping it unmaps the file, unless it was
/// kept for the life of the process with `leak`.
pub struct Mapped {
    data: &'static [u8],
}

impl Mapped {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| io::Error::other("too large to map"))?;
        if len == 0 {
            return Ok(Self { data: &[] });
        }
        Self::map(&file, len)
    }

    #[cfg(unix)]
    fn map(file: &File, len: usize) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        // SAFETY: a fresh read-only, private mapping of an open file of
        // `len` bytes; it stays valid until `munmap` in `drop`.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the mapping above is `len` readable bytes.
        let data = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        Ok(Self { data })
    }

    /// Without `mmap`, the file is read: fonts are rarely large there.
    #[cfg(not(unix))]
    fn map(file: &File, len: usize) -> io::Result<Self> {
        use std::io::Read;
        let mut data = Vec::with_capacity(len);
        let mut file = file;
        file.read_to_end(&mut data)?;
        Ok(Self {
            data: Box::leak(data.into_boxed_slice()),
        })
    }

    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// Keeps the mapping for the life of the process: fonts in use are
    /// never unloaded.
    pub fn leak(self) -> &'static [u8] {
        let data = self.data;
        std::mem::forget(self);
        data
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        #[cfg(unix)]
        if !self.data.is_empty() {
            // SAFETY: `data` is exactly the mapping made in `map`, unmapped
            // once.
            unsafe {
                libc::munmap(self.data.as_ptr().cast_mut().cast(), self.data.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_what_reading_would_read() {
        let path = std::env::temp_dir().join(format!("rubychat-mmap-{}", std::process::id()));
        std::fs::write(&path, b"mapped bytes").unwrap();
        let mapped = Mapped::open(&path).unwrap();
        assert_eq!(mapped.data(), b"mapped bytes");
        drop(mapped);
        std::fs::write(&path, b"").unwrap();
        assert!(Mapped::open(&path).unwrap().data().is_empty());
        std::fs::remove_file(&path).unwrap();
        assert!(Mapped::open(&path).is_err());
    }
}
