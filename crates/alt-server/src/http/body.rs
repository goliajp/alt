//! A request body received into a bounded temporary file.
use super::*;

pub(super) enum BodyError {
    Limit,
    Malformed,
    Storage,
}
impl From<std::io::Error> for BodyError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
pub(super) async fn spool(
    body: Incoming,
    max: u64,
) -> Result<alt_os::temp::NamedTempFile, BodyError> {
    let mut guard = observation::current().map(|trace| trace.phase(Phase::Upload));
    let result = spool_inner(body, max, guard.as_mut()).await;
    if let Some(guard) = guard {
        guard.finish(match &result {
            Ok(_) => Outcome::Ok,
            Err(BodyError::Limit) => Outcome::Limit,
            Err(BodyError::Storage) => Outcome::Unavailable,
            Err(BodyError::Malformed) => Outcome::Invalid,
        });
    }
    result
}
pub(super) async fn spool_inner(
    mut body: Incoming,
    max: u64,
    mut guard: Option<&mut alt_server_telemetry::PhaseGuard>,
) -> Result<alt_os::temp::NamedTempFile, BodyError> {
    let temp = alt_os::temp::NamedTempFile::new()?;
    let mut file = tokio::fs::File::from_std(temp.as_file().try_clone()?);
    let mut remaining = max;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| BodyError::Malformed)?;
        let data = frame.into_data().map_err(|_| BodyError::Malformed)?;
        if let Some(guard) = &mut guard {
            guard.add_bytes(data.len() as u64);
        }
        remaining = remaining
            .checked_sub(data.len() as u64)
            .ok_or(BodyError::Limit)?;
        file.write_all(&data).await?;
    }
    file.flush().await?;
    Ok(temp)
}
