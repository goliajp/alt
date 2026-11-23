//! A request body received into a bounded temporary file.
//!
//! A body has the time bodies are given, as a whole, and has to keep coming: in
//! each [`WINDOW`] that passes, [`RATE`] bytes a second on average. One that stalls
//! is given up within two of those, however fast it came before; one that is as
//! slow as its line and goes on has all the time bodies are given.
use super::*;

/// the bytes a second a body has to average over a window
const RATE: u64 = 1024;
/// the time in which a body has to have moved
const WINDOW: Duration = Duration::from_secs(30);

/// Whether a body goes on: what came of it in the window that is running.
struct Progress {
    until: tokio::time::Instant,
    moved: u64,
}
impl Progress {
    fn from(now: tokio::time::Instant) -> Self {
        let (until, moved) = (now + WINDOW, 0);
        Progress { until, moved }
    }
    /// The end of the window `now` is in. Where the one before is just over, too
    /// little having come in it is the end of the body.
    fn window(&mut self, now: tokio::time::Instant) -> Result<tokio::time::Instant, BodyError> {
        if now >= self.until {
            if self.moved < RATE * WINDOW.as_secs() {
                return Err(BodyError::Slow);
            }
            *self = Progress::from(now);
        }
        Ok(self.until)
    }
}

pub(super) enum BodyError {
    Limit,
    Malformed,
    Storage,
    /// it stalled, or the time of a body is over
    Slow,
    /// the instance holds as many bytes of bodies as it may
    Full,
}
impl From<std::io::Error> for BodyError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
/// How a body is received: within what times, and counted where.
pub(super) struct Receipt<'a> {
    pub(super) options: &'a HttpOptions,
    pub(super) slot: Option<&'a mut receiving::Slot>,
}
impl<'a> Receipt<'a> {
    pub(super) fn of(state: &'a App, slot: Option<&'a mut receiving::Slot>) -> Self {
        let options = &state.options;
        Receipt { options, slot }
    }
}
pub(super) async fn spool(
    body: Body,
    max: u64,
    receipt: Receipt<'_>,
) -> Result<alt_os::temp::NamedTempFile, BodyError> {
    let mut guard = observation::current().map(|trace| trace.phase(Phase::Upload));
    let whole = receipt.options.body_timeout;
    let received = spool_inner(body, max, guard.as_mut(), receipt);
    let result = match tokio::time::timeout(whole, received).await {
        Ok(result) => result,
        Err(_) => Err(BodyError::Slow),
    };
    if let Some(guard) = guard {
        guard.finish(match &result {
            Ok(_) => Outcome::Ok,
            Err(BodyError::Limit) => Outcome::Limit,
            Err(BodyError::Storage | BodyError::Slow | BodyError::Full) => Outcome::Unavailable,
            Err(BodyError::Malformed) => Outcome::Invalid,
        });
    }
    result
}
async fn spool_inner(
    mut body: Body,
    max: u64,
    mut guard: Option<&mut alt_server_telemetry::PhaseGuard>,
    mut receipt: Receipt<'_>,
) -> Result<alt_os::temp::NamedTempFile, BodyError> {
    let temp = alt_os::temp::NamedTempFile::new()?;
    let mut file = tokio::fs::File::from_std(temp.as_file().try_clone()?);
    let mut remaining = max;
    let mut progress = Progress::from(tokio::time::Instant::now());
    loop {
        // a wait that ends with the window goes on in the next, where
        // enough came in the one that is over
        let until = progress.window(tokio::time::Instant::now())?;
        let Ok(frame) = tokio::time::timeout_at(until, body.frame()).await else {
            continue;
        };
        let Some(frame) = frame else {
            break;
        };
        let frame = frame.map_err(|_| BodyError::Malformed)?;
        let data = frame.into_data().map_err(|_| BodyError::Malformed)?;
        if let Some(guard) = &mut guard {
            guard.add_bytes(data.len() as u64);
        }
        progress.moved += data.len() as u64;
        remaining = remaining
            .checked_sub(data.len() as u64)
            .ok_or(BodyError::Limit)?;
        if let Some(slot) = &mut receipt.slot {
            slot.took(data.len() as u64).map_err(|_| BodyError::Full)?;
        }
        file.write_all(&data).await?;
    }
    file.flush().await?;
    Ok(temp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_goes_on_while_it_moves_and_what_came_fast_buys_no_time_to_stall_in() {
        let began = tokio::time::Instant::now();
        let mut progress = Progress::from(began);
        // as slow as it may be, for a day
        let mut now = began;
        for _ in 0..2880 {
            assert!(
                progress
                    .window(now)
                    .is_ok_and(|until| until == now + WINDOW)
            );
            progress.moved = RATE * WINDOW.as_secs();
            now += WINDOW;
        }
        // a GiB in one window, and nothing in the next
        assert!(progress.window(now).is_ok());
        progress.moved = 1 << 30;
        assert!(
            progress
                .window(now + WINDOW - Duration::from_millis(1))
                .is_ok()
        );
        assert!(progress.window(now + WINDOW).is_ok());
        let stalled = progress.window(now + 2 * WINDOW);
        assert!(matches!(stalled, Err(BodyError::Slow)));
        // one byte short of what a window asks for
        let mut progress = Progress::from(began);
        progress.moved = RATE * WINDOW.as_secs() - 1;
        assert!(matches!(
            progress.window(began + WINDOW),
            Err(BodyError::Slow)
        ));
    }
}
