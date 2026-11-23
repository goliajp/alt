//! The bytes of a base while deltas of it wait: in memory, or in a file
//! of their own once memory is to be made room in.

use std::io::Write;
use std::ops::Deref;

use alt_os::map::Mmap;

pub(super) enum Base {
    Held(Vec<u8>),
    Spilled(Mmap),
}

impl Deref for Base {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Base::Held(bytes) => bytes,
            Base::Spilled(map) => map,
        }
    }
}

impl Base {
    /// The bytes this takes of the memory bases are held in.
    pub(super) fn held(&self) -> usize {
        match self {
            Base::Held(bytes) => bytes.len(),
            Base::Spilled(_) => 0,
        }
    }

    /// The bytes this takes of the room bases are spilled to.
    pub(super) fn spilled(&self) -> u64 {
        match self {
            Base::Held(_) => 0,
            Base::Spilled(map) => map.len() as u64,
        }
    }

    /// The same bytes in a file that has no name any more.
    pub(super) fn spill(&self) -> std::io::Result<Base> {
        let dir = alt_os::temp::tempdir()?;
        let path = dir.path().join("base");
        let mut file = std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(self)?;
        // the file is this pack's own and is not written to again
        let map = unsafe { Mmap::map(&file)? };
        Ok(Base::Spilled(map))
    }
}
