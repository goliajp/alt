//! What a test has a log do that no machine does on its own: run in a
//! boot of its choosing or in none, and have a cut of the log die or
//! wait at one of its steps.
//!
//! All of it is built with the feature `hooks` alone, which the tests of
//! this crate turn on for themselves through its dev-dependencies, as a
//! crate does whose tests need it. Without the feature a handle runs in
//! the boot the kernel tells, a cut runs through, and none of what is
//! kept here per thread is in the build.

#[cfg(feature = "hooks")]
mod told {
    use std::cell::{Cell, RefCell};
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

    thread_local! {
        static BOOT: Cell<Told> = const { Cell::new(Told::ByTheKernel) };
        static CUT_DIES: Cell<u8> = const { Cell::new(0) };
        static CUT_WAITS: RefCell<Option<Waits>> = const { RefCell::new(None) };
        static ASKED_WAITS: RefCell<Option<(Sender<()>, Receiver<()>)>> =
            const { RefCell::new(None) };
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
pub(crate) use told::{cut_asked, cut_reached, running_boot};
#[cfg(feature = "hooks")]
pub use told::{cut_asked_waits, cut_dies_after, cut_waits_after, in_boot, in_no_boot};

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
