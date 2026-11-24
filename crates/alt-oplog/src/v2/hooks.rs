//! What a test has a log do that no machine does on its own: run in a
//! boot of its choosing or in none, have a cut of the log die or wait at
//! one of its steps, and have something done to `tail` before each read
//! of it.
//!
//! All of it is built with the feature `hooks` alone, which no manifest
//! turns on: the tests of this crate need it, and are built with it
//! named on the command line. Without the feature a handle runs in the
//! boot the kernel tells, a cut runs through, and none of what is kept
//! here per thread is in the build.

#[cfg(feature = "hooks")]
mod told {
    use std::cell::{Cell, RefCell};
    use std::path::Path;
    use std::sync::mpsc::{Receiver, Sender};

    /// What a thread takes for the boot it runs in.
    #[derive(Clone, Copy)]
    enum Told {
        ByTheKernel,
        This([u8; 16]),
        Nothing,
    }

    /// The step of a cut a test has it wait after, whom it tells that it
    /// is there, and who tells it to go on.
    type Waits = (u8, Sender<()>, Receiver<()>);

    /// What a test has done in the directory of a log before `tail`
    /// there is read.
    type BeforeRead = Box<dyn FnMut(&Path)>;

    thread_local! {
        static NOTE_READ: RefCell<Option<BeforeRead>> = const { RefCell::new(None) };
        static BOOT: Cell<Told> = const { Cell::new(Told::ByTheKernel) };
        static CUT_DIES: Cell<u8> = const { Cell::new(0) };
        static CUT_WAITS: RefCell<Option<Waits>> = const { RefCell::new(None) };
        static ASKED_WAITS: RefCell<Option<(Sender<()>, Receiver<()>)>> =
            const { RefCell::new(None) };
        static TAKEN_BACK: RefCell<Option<BeforeRead>> = const { RefCell::new(None) };
    }

    /// Makes the handles this thread opens from now on take `boot` for
    /// the boot they run in, and the running one again with `None`: for
    /// a test of what a log is taken for after the machine went down.
    pub fn in_boot(boot: Option<[u8; 16]>) {
        BOOT.set(boot.map_or(Told::ByTheKernel, Told::This));
    }

    /// Makes the handles this thread opens from now on run as on a
    /// system whose kernel tells no boot: for a test of that.
    pub fn in_no_boot() {
        BOOT.set(Told::Nothing);
    }

    /// Makes a cut of a log on this thread end after step `step` of it
    /// as if its process had died there (1 the log and `tail` told of it,
    /// 2 the checkpoint, 3 its sync, 4 the file made shorter, 5 the sync
    /// of that), and every cut run through with `None`: for a test of
    /// what such a death leaves.
    pub fn cut_dies_after(step: Option<u8>) {
        CUT_DIES.set(step.unwrap_or(0));
    }

    /// Makes the next cut of a log on this thread stop after step `step`
    /// of it, with its locks held: it says so on `reached` and goes on
    /// when `go` is sent to or closed. For a test of what others find
    /// while a log is being cut.
    pub fn cut_waits_after(step: u8, reached: Sender<()>, go: Receiver<()>) {
        CUT_WAITS.set(Some((step, reached, go)));
    }

    /// Makes the next handle on this thread that asks, with no lock on
    /// the log, whether frames it holds were cut stop between the two
    /// things it looks at for that: it says so on `reached` and goes on
    /// when `go` is sent to or closed. For a test of a cut that is made
    /// in between.
    pub fn cut_asked_waits(reached: Sender<()>, go: Receiver<()>) {
        ASKED_WAITS.set(Some((reached, go)));
    }

    /// Has `first` done in the directory of a log before every read of
    /// `tail` there that a handle of this thread makes, and nothing
    /// with `None`: for a test of a note that is gone or spoiled at any
    /// moment.
    pub fn before_note_read(first: Option<BeforeRead>) {
        NOTE_READ.set(first);
    }

    /// Has `first` done in the directory of a log before a handle of
    /// this thread takes back a cut the header of the log tells of,
    /// with the locks it holds for that held, and nothing with `None`:
    /// for a test of what it holds then.
    pub fn before_cut_taken_back(first: Option<BeforeRead>) {
        TAKEN_BACK.set(first);
    }

    pub(crate) fn cut_taken_back(dir: &Path) {
        TAKEN_BACK.with_borrow_mut(|first| {
            if let Some(first) = first {
                first(dir);
            }
        });
    }

    pub(crate) fn note_read(dir: &Path) {
        NOTE_READ.with_borrow_mut(|first| {
            if let Some(first) = first {
                first(dir);
            }
        });
    }

    pub(crate) fn cut_asked() {
        if let Some((reached, go)) = ASKED_WAITS.take() {
            let _ = reached.send(());
            let _ = go.recv();
        }
    }

    pub(crate) fn running_boot() -> Option<[u8; 16]> {
        match BOOT.get() {
            Told::ByTheKernel => alt_os::boot::id(),
            Told::This(boot) => Some(boot),
            Told::Nothing => None,
        }
    }

    pub(crate) fn cut_reached(step: u8) -> std::io::Result<()> {
        if CUT_DIES.get() == step {
            return Err(std::io::Error::other("the cut was made to die here"));
        }
        let waits =
            CUT_WAITS.with_borrow(|waits| waits.as_ref().is_some_and(|(at, ..)| *at == step));
        if waits && let Some((_, reached, go)) = CUT_WAITS.take() {
            let _ = reached.send(());
            let _ = go.recv();
        }
        Ok(())
    }
}

#[cfg(feature = "hooks")]
pub use told::{
    before_cut_taken_back, before_note_read, cut_asked_waits, cut_dies_after, cut_waits_after,
    in_boot, in_no_boot,
};
#[cfg(feature = "hooks")]
pub(crate) use told::{cut_asked, cut_reached, cut_taken_back, note_read, running_boot};

/// The boot a handle opened now runs in, as the kernel tells it.
#[cfg(not(feature = "hooks"))]
pub(crate) fn running_boot() -> Option<[u8; 16]> {
    alt_os::boot::id()
}

/// A cut has done step `step` of its steps.
#[cfg(not(feature = "hooks"))]
#[inline(always)]
pub(crate) fn cut_reached(_step: u8) -> std::io::Result<()> {
    Ok(())
}

/// A handle that holds no lock on the log is between the two things it
/// looks at to tell whether frames it holds were cut.
#[cfg(not(feature = "hooks"))]
#[inline(always)]
pub(crate) fn cut_asked() {}

/// A cut the header of the log in `dir` tells of is about to be taken
/// back.
#[cfg(not(feature = "hooks"))]
#[inline(always)]
pub(crate) fn cut_taken_back(_dir: &std::path::Path) {}

/// `tail` in `dir` is about to be read.
#[cfg(not(feature = "hooks"))]
#[inline(always)]
pub(crate) fn note_read(_dir: &std::path::Path) {}
