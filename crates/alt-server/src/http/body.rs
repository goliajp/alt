//! A request body received into a bounded temporary file.
//!
//! A body has the time bodies are given, as a whole, and has to keep coming: once
//! the time a head is given is over it has to have averaged [`RATE`] bytes a second,
//! so that one that stalls is given up long before the whole time is.
use super::*;

/// the bytes a second a body has to average once the time of a head is over
const RATE: u64 = 1024;

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
    let began = tokio::time::Instant::now();
    let grace = receipt.options.header_timeout;
    loop {
        let earned = Duration::from_millis((max - remaining).saturating_mul(1000) / RATE);
        let frame = tokio::time::timeout_at(began + grace + earned, body.frame()).await;
        let Some(frame) = frame.map_err(|_| BodyError::Slow)? else {
            break;
        };
        let frame = frame.map_err(|_| BodyError::Malformed)?;
        let data = frame.into_data().map_err(|_| BodyError::Malformed)?;
        if let Some(guard) = &mut guard {
            guard.add_bytes(data.len() as u64);
        }
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
