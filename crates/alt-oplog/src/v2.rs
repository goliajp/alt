//! Version 2 of the log file: frames in blocks, with checkpoints.
//!
//! The log is the only file that is synced. Every frame says up to where
//! its writer knew the log synced, so the boundary is in the log itself;
//! `tail`, beside it, holds what a boot knows of the log and is good for
//! that boot alone. A reader begins at the last checkpoint and reads no
//! more than a block, once somebody has read through what an earlier boot
//! left unsynced at the end.

mod append;
pub mod block;
pub mod checkpoint;
pub mod compose;
mod cut;
mod cuts;
mod ext;
mod follow;
pub mod frame;
mod lift;
mod locate;
mod log;
mod named;
mod notes;
mod open;
mod passed;
pub mod receipt;
mod reread;
mod salvage;
pub mod scan;
mod since;
mod sink;
mod source;
pub mod state;
mod stream;
mod sync;
mod tail;
mod verify;

use std::cell::{Cell, RefCell};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};

pub(crate) use log::Log;
pub use sink::Reached;
pub(crate) use sink::Sink;

use crate::lock::{lock_exclusive, unlock};
use crate::v2::frame::{OdbBounds, Step};

/// What the frame of an op says of it beside its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// the first bytes of the id of the workspace the op belongs to
    pub workspace: Option<[u8; 8]>,
    pub undoable: bool,
    /// nothing is appended after the op by an alt that does not know it
    pub required: bool,
    /// a ref transaction: its frame names the one before it
    pub linked: bool,
    /// the frame of the op this one undoes
    pub inverse_of: Option<u64>,
    pub request: Option<[u8; 16]>,
    /// the checksum that closes the workspace's index once the op is done
    pub index_after: Option<[u8; 20]>,
    pub step: Option<Step>,
    /// the workspace is gone with this op
    pub retired: bool,
    /// how far the object store is synced
    pub odb: Option<OdbBounds>,
}

/// The body of an op that follows the op `parent`: what an op frame holds
/// and what the op's id is the hash of.
pub fn op_body(parent: &crate::OpId, timestamp_ms: u64, actor: &str, payload: &[u8]) -> Vec<u8> {
    crate::scan::encode_body(parent, timestamp_ms, actor, payload)
}

/// Creates an empty version 2 log in `dir` with blocks of the usual size,
/// unless a log is there already.
pub fn create(dir: &Path) -> std::io::Result<()> {
    create_with(dir, block::BLOCK_EXP)
}

/// [`create`] with blocks of two to the power of `block_exp` bytes.
pub fn create_with(dir: &Path, block_exp: u8) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("log"))?;
    lock_exclusive(&file)?;
    let created = (|| {
        if file.metadata()?.len() != 0 {
            return Ok(());
        }
        let mut id = [0u8; 16];
        alt_os::random::fill(&mut id);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        let header = block::FileHeader {
            block_exp,
            incompatible: block::DRAFT,
            read_compatible: 0,
            compatible: 0,
            id,
            created_ms: now.map_or(0, |since| since.as_millis() as u64),
        };
        file.write_all(&header.encode())?;
        file.sync_all()?;
        // what is appended from now on is appended in this boot
        tail::Tail::update(dir, running_boot(), |tail| {
            tail.judged = block::FILE_HEADER_LEN as u64;
        })
    })();
    let _ = unlock(&file);
    created
}

/// What a thread takes for the boot it runs in.
#[derive(Clone, Copy)]
enum Told {
    ByTheKernel,
    This([u8; 16]),
    Nothing,
}

thread_local! {
    static BOOT: Cell<Told> = const { Cell::new(Told::ByTheKernel) };
}

/// Makes the handles this thread opens from now on take `boot` for the
/// boot they run in, and the running one again with `None`: for a test
/// of what a log is taken for after the machine went down.
#[doc(hidden)]
pub fn in_boot(boot: Option<[u8; 16]>) {
    BOOT.set(boot.map_or(Told::ByTheKernel, Told::This));
}

/// Makes the handles this thread opens from now on run as on a system
/// whose kernel tells no boot: for a test of that.
#[doc(hidden)]
pub fn in_no_boot() {
    BOOT.set(Told::Nothing);
}

thread_local! {
    static CUT_DIES: Cell<u8> = const { Cell::new(0) };
}

/// Makes a cut of a log on this thread end after step `step` of it as if
/// its process had died there (1 the note in `tail`, 2 the checkpoint, 3
/// its sync, 4 the file made shorter, 5 the sync of that), and every cut
/// run through with `None`: for a test of what such a death leaves.
#[doc(hidden)]
pub fn cut_dies_after(step: Option<u8>) {
    CUT_DIES.set(step.unwrap_or(0));
}

/// The step of a cut a test has it wait after, whom it tells that it is
/// there, and who tells it to go on.
type Waits = (u8, Sender<()>, Receiver<()>);

thread_local! {
    static CUT_WAITS: RefCell<Option<Waits>> = const { RefCell::new(None) };
}

/// Makes the next cut of a log on this thread stop after step `step` of
/// it, with its locks held: it says so on `reached` and goes on when
/// `go` is sent to or closed. For a test of what others find while a log
/// is being cut.
#[doc(hidden)]
pub fn cut_waits_after(step: u8, reached: Sender<()>, go: Receiver<()>) {
    CUT_WAITS.set(Some((step, reached, go)));
}

/// Fails where a test has the cut die after step `step`, and waits where
/// a test has it wait there.
pub(crate) fn cut_reached(step: u8) -> std::io::Result<()> {
    if CUT_DIES.get() == step {
        return Err(std::io::Error::other("the cut was made to die here"));
    }
    let waits = CUT_WAITS.with_borrow(|waits| waits.as_ref().is_some_and(|(at, ..)| *at == step));
    if waits && let Some((_, reached, go)) = CUT_WAITS.take() {
        let _ = reached.send(());
        let _ = go.recv();
    }
    Ok(())
}

/// The boot a handle opened now on this thread runs in; `None` where the
/// kernel tells none, and nothing a boot left in its cache is then taken
/// on trust.
pub fn running_boot() -> Option<[u8; 16]> {
    match BOOT.get() {
        Told::ByTheKernel => alt_os::boot::id(),
        Told::This(boot) => Some(boot),
        Told::Nothing => None,
    }
}

/// Up to where the log in `dir` is known synced in the running boot.
pub(crate) fn bounded_in(dir: &Path) -> Option<u64> {
    tail::Tail::read(dir, running_boot()).map(|tail| tail.synced)
}
