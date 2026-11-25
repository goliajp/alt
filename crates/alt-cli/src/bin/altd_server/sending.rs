//! The room the responses to fetches have on the disk between them.
//!
//! A response to a fetch is a file until its peer has taken it, and a peer
//! takes it as slowly as its line: the files that are there at once are as
//! many as the connections, each as large as what its fetch asked for.
//! Together they take no more than `ALT_SERVER_MAX_SENDING_BYTES`, and
//! those of one address no more than a quarter of that. A response that
//! would take them past either is not written, and its fetch is told to
//! come again. One that is alone is written whatever its size, and the
//! only one of its address as far as the room goes, so that a repository
//! larger than the room can still be cloned.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::http::{Share, Shares};

/// What a fetch is answered with when its response has no room.
pub(super) const FULL: &str = "the responses being sent take all the room they have; fetch again";

/// The bytes the responses there are take, and the most they may.
pub(super) struct Room(Arc<Shares>);

impl Room {
    /// `most` bytes, of which `large` responses at once may have more than the
    /// part of their address, taken at `rate` at the least while another waits.
    pub(super) fn of(most: u64, large: usize, rate: u64) -> Self {
        let window = alt_shares::LARGE_WINDOW;
        Room(Shares::passed_by_one_alone(most, large).paced(rate, window))
    }

    /// The room all the responses of this server have.
    pub(super) fn shared() -> &'static Room {
        static ROOM: std::sync::OnceLock<Room> = std::sync::OnceLock::new();
        let told = super::told::told();
        let (most, large, rate) = (told.sending_bytes, told.workers / 2, told.large_rate);
        ROOM.get_or_init(|| Room::of(most, large, rate))
    }

    /// Has `file` written within the room, for a peer counted under `from`
    /// that is gone where its mark is set: where it would be large, it waits
    /// for its turn until `by` while the peer is there.
    pub(super) fn writing(
        &self,
        file: File,
        (from, gone): (Option<IpAddr>, &Arc<AtomicBool>),
        by: Option<Instant>,
    ) -> Written {
        let taken = self.0.share(from).waiting(by, Some(Arc::clone(gone)));
        Written { file, taken }
    }

    #[cfg(test)]
    pub(super) fn held(&self) -> u64 {
        self.0.held()
    }
}

/// The file of a response while it is written.
pub(super) struct Written {
    file: File,
    taken: Share,
}

impl Write for Written {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.taken.take(buf.len() as u64).is_err() {
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
    taken: Share,
}

impl Sent {
    pub(super) fn length(&self) -> u64 {
        self.taken.bytes()
    }
}

impl Read for Sent {
    /// A large response another waits for its turn behind goes on only as fast
    /// as such a one has to.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read(buf)?;
        match self.taken.sent(read as u64) {
            Ok(()) => Ok(read),
            Err(slow) => Err(io::Error::new(io::ErrorKind::TimedOut, slow)),
        }
    }
}

/// Half of what is free where the files of responses are written.
pub(super) fn half_of_what_is_free() -> io::Result<u64> {
    alt_shares::half_of_what_is_free(&std::env::temp_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(room: &Room, bytes: usize) -> io::Result<Written> {
        written_from(room, None, bytes)
    }

    fn written_from(room: &Room, from: Option<u8>, bytes: usize) -> io::Result<Written> {
        let from = from.map(|from| IpAddr::from([192, 0, 2, from]));
        let gone = Arc::new(AtomicBool::new(false));
        let mut file = room.writing(alt_os::temp::tempfile()?, (from, &gone), None);
        file.write_all(&vec![7; bytes])?;
        Ok(file)
    }

    #[test]
    fn the_responses_there_are_take_no_more_than_their_room_and_one_alone_whatever_it_is() {
        let room = Room::of(1000, 1, alt_shares::LARGE_RATE);
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

    #[test]
    fn the_responses_to_one_address_take_a_quarter_of_the_room_and_leave_the_rest() {
        let room = Room::of(1000, 1, alt_shares::LARGE_RATE);
        let paced = Room::of(1000, 1, 256 << 10).0.pace();
        assert_eq!(paced, (256 << 10, alt_shares::LARGE_WINDOW));
        let slow: Vec<Written> = (0..5)
            .map(|_| written_from(&room, Some(1), 50).unwrap())
            .collect();
        let refused = written_from(&room, Some(1), 1).map(|_| ()).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::StorageFull);
        // another address is answered while the first has all it may
        let other = written_from(&room, Some(2), 250).unwrap();
        assert_eq!(room.held(), 500);
        drop((slow, other));
        assert_eq!(room.held(), 0);
    }
}
