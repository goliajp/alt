//! How long a fetch is gone on with.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// How long a fetch is gone on with: for as long as one may take, and for
/// as long as its peer waits for it. A fetch reads what its sender names,
/// as much as the repository holds; it has a worker while it does.
pub(super) struct Until<'a> {
    pub(super) by: Option<Instant>,
    pub(super) gone: Option<&'a AtomicBool>,
}

/// Why a fetch was gone on with no longer.
#[derive(Debug, PartialEq)]
pub(super) enum Stopped {
    Late,
    Gone,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Stopped::Late => "the fetch takes longer than the server gives one",
            Stopped::Gone => "the peer of the fetch is gone",
        })
    }
}

impl std::error::Error for Stopped {}

impl Until<'_> {
    /// A fetch nobody waits for and that has no time: gone on with to its end.
    #[cfg(test)]
    pub(super) fn whenever() -> Until<'static> {
        Until {
            by: None,
            gone: None,
        }
    }

    /// Asked before each object a fetch reads.
    pub(super) fn go_on(&self) -> Result<(), Stopped> {
        if self.gone.is_some_and(|gone| gone.load(Ordering::Relaxed)) {
            return Err(Stopped::Gone);
        }
        match self.by.is_some_and(|by| Instant::now() >= by) {
            true => Err(Stopped::Late),
            false => Ok(()),
        }
    }
}
