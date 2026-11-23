//! What requests reserved and never used, given back as the server runs.
//!
//! A request reserves room for what it is going to upload. One that fails, is
//! given up, or loses its connection keeps that room until its time to upload is
//! over; from then on nothing but an end put to it frees the room. That end is
//! put here, every so often, by the server itself, on a connection of its own
//! that it keeps for the round: after a restart the first round finds what the
//! last process left.

use super::*;
use alt_server_telemetry::BackgroundLog;

/// the requests ended in one repository in one round
const BATCH: u32 = 1000;

pub(super) async fn run(state: Arc<App>) {
    let log = BackgroundLog::current();
    loop {
        tokio::time::sleep(state.options.expiry_interval).await;
        let factory = (*state.factory).clone();
        let ended = spawn_blocking(move || factory.open()?.expire_requests(BATCH)).await;
        if let Ok(Err(error)) = ended {
            log.failed("request expiry", "", &error);
        }
    }
}
