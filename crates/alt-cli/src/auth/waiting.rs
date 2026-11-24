//! Waiting for a device request to be confirmed.
use super::*;

/// Tells where the request `pending` is confirmed and waits until it is: the
/// credential that is then issued is saved at `issued_path`. With `no_wait`
/// nothing is waited for, and what a caller that comes again is told is
/// returned.
pub(super) fn awaited(
    client: &Client,
    pending: &Pending,
    (issued_path, path): (&Path, &Path),
    no_wait: bool,
    progress: &mut impl Write,
) -> Res<Option<alt_json::Value>> {
    let time = now()?;
    let end = pending
        .started_at_unix_ms
        .saturating_add(pending.authorization.expires_in.saturating_mul(1000));
    if time < pending.started_at_unix_ms || time >= end {
        return Err(refused(
            "device request expired; no new request was started. Inspect terminal credentials in Settings before starting another login",
        ));
    }
    writeln!(
        progress,
        "Open {}\nConfirm terminal code: {}",
        pending.authorization.verification_uri_complete, pending.authorization.user_code
    )?;
    progress.flush()?;
    if no_wait {
        return Ok(Some(
            json!({"schema_version":1,"status":"authorization_pending","verification_uri":pending.authorization.verification_uri,"verification_uri_complete":pending.authorization.verification_uri_complete,"user_code":pending.authorization.user_code,"credential_file":path,"resume":true}),
        ));
    }
    let deadline = Instant::now() + Duration::from_millis(end - time);
    let mut interval = Duration::from_secs(pending.authorization.interval);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining <= interval {
            return Err(refused(
                "device request expired; pending state was retained",
            ));
        }
        std::thread::sleep(interval);
        let started = now()?;
        match client.poll(&pending.authorization.device_code) {
            Ok(value) => {
                save(
                    issued_path,
                    &Issued {
                        version: 1,
                        device_digest: alt_hash::blake3::hash(
                            pending.authorization.device_code.as_bytes(),
                        )
                        .to_hex()
                        .to_string(),
                        access_token: value.access_token,
                        expires_at_unix_ms: started.saturating_add(value.expires_in * 1000),
                    },
                )
                .map_err(|_| {
                    unconfirmed(
                        "issued credential save is unconfirmed; resume the same login journal",
                    )
                })?;
                return Ok(None);
            }
            Err(DeviceTransportError::Rejected(DeviceError::AuthorizationPending)) => (),
            Err(DeviceTransportError::Rejected(DeviceError::SlowDown)) => {
                interval = interval.saturating_add(Duration::from_secs(5))
            }
            Err(
                DeviceTransportError::Unavailable
                | DeviceTransportError::Rejected(DeviceError::TemporarilyUnavailable),
            ) => interval = interval.saturating_mul(2).min(Duration::from_secs(60)),
            Err(error) => return Err(error.into()),
        }
    }
}
