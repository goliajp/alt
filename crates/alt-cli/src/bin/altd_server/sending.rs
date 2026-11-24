//! The room the responses to fetches have on the disk between them.
//!
//! A response to a fetch is a file until its peer has taken it, and a peer
//! takes it as slowly as its line: the files that are there at once are as
//! many as the connections, each as large as what its fetch asked for.
//! Together they take no more than `ALT_SERVER_MAX_SENDING_BYTES`. A
//! response that would take them past it is not written, and its fetch is
//! told to come again; one that is alone is written whatever its size, so
//! that a repository larger than the room can still be cloned.

use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// What a fetch is answered with when its response has no room.
pub(super) const FULL: &str = "the responses being sent take all the room they have; fetch again";

/// The bytes the responses there are take, and the most they may.
pub(super) struct Room {
    held: Arc<AtomicU64>,
    most: u64,
}

/// What one response has taken of the room, given back when it is gone.
pub(super) struct Taken {
    bytes: u64,
    most: u64,
    of: Arc<AtomicU64>,
}

impl Room {
    pub(super) fn of(most: u64) -> Self {
        let held = Arc::new(AtomicU64::new(0));
        Room { held, most }
    }

    /// The room all the responses of this server have.
    pub(super) fn shared() -> &'static Room {
        static ROOM: std::sync::OnceLock<Room> = std::sync::OnceLock::new();
        ROOM.get_or_init(|| Room::of(super::told::told().sending_bytes))
    }

    /// Has `file` written within the room.
    pub(super) fn writing(&self, file: File) -> Written {
        let (bytes, most, of) = (0, self.most, Arc::clone(&self.held));
        Written {
            file,
            taken: Taken { bytes, most, of },
        }
    }

    #[cfg(test)]
    pub(super) fn held(&self) -> u64 {
        self.held.load(Ordering::Acquire)
    }
}

impl Drop for Taken {
    fn drop(&mut self) {
        self.of.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// The file of a response while it is written.
pub(super) struct Written {
    file: File,
    taken: Taken,
}

impl Write for Written {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let bytes = buf.len() as u64;
        let held = self.taken.of.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.taken.bytes += bytes;
        if held > self.taken.most && held > self.taken.bytes {
            return Err(io::Error::new(io::ErrorKind::StorageFull, FULL));
        }
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Written {
    /// What was written, to be read out of `file`, which is the same file
    /// opened from its beginning.
    pub(super) fn sent_from(self, file: File) -> Sent {
        Sent {
            file,
            taken: self.taken,
        }
    }
}

/// The file of a response while it is sent.
pub(super) struct Sent {
    file: File,
    taken: Taken,
}

impl Sent {
    pub(super) fn length(&self) -> u64 {
        self.taken.bytes
    }
}

impl Read for Sent {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

/// Half of what is free where the files of responses are written.
pub(super) fn half_of_what_is_free() -> io::Result<u64> {
    use std::os::fd::AsRawFd;
    let directory = File::open(std::env::temp_dir())?;
    let mut value = std::mem::MaybeUninit::<alt_os::sys::statvfs>::uninit();
    // SAFETY: fstatvfs writes the structure it is pointed to where it succeeds,
    // and the descriptor is open through the call
    if unsafe { alt_os::sys::fstatvfs(directory.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded and filled every field
    let value = unsafe { value.assume_init() };
    Ok(((value.f_frsize as u128 * value.f_bavail as u128) / 2).min(u64::MAX as u128) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(room: &Room, bytes: usize) -> io::Result<Written> {
        let mut file = room.writing(alt_os::temp::tempfile()?);
        file.write_all(&vec![7; bytes])?;
        Ok(file)
    }

    #[test]
    fn the_responses_there_are_take_no_more_than_their_room_and_one_alone_whatever_it_is() {
        let room = Room::of(1000);
        let first = written(&room, 600).unwrap();
        let second = written(&room, 400).unwrap();
        assert_eq!(room.held(), 1000);
        // a third has no room, and what it took before it was refused is given back
        let refused = written(&room, 1).map(|_| ()).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::StorageFull);
        assert_eq!(room.held(), 1000);
        // the room is taken until a response is sent, not until it is written
        let first = first.sent_from(alt_os::temp::tempfile().unwrap());
        assert_eq!((first.length(), room.held()), (600, 1000));
        assert!(written(&room, 1).is_err());
        drop(first);
        assert_eq!(room.held(), 400);
        written(&room, 600).unwrap();
        drop(second);
        assert_eq!(room.held(), 0);
        // alone it is written though it is larger than the room, and nothing beside it
        let large = written(&room, 5000).unwrap();
        assert!(written(&room, 1).is_err());
        drop(large);
        assert_eq!(room.held(), 0);
    }
}
