//! What requests reserved and never used, given back as the server runs.
//!
//! A request reserves room for what it is going to upload. One that fails, is
//! given up, or loses its connection keeps that room until its time to upload is
//! over; from then on nothing but an end put to it frees the room. That end is
//! put here, every so often, by the server itself, on a connection of its own
//! that it keeps for the round: after a restart the first round finds what the
//! last process left.
//!
//! The same rounds remove what has been over for long enough to be let go of:
//! requests, sessions and credentials each have their time, which the store keeps.

use super::*;
use alt_server_store::RequestsSeen;
use alt_server_telemetry::BackgroundLog;

/// the requests ended in one repository in one round
const BATCH: u32 = 1000;
/// the rows one rule removes in one round of removing what is over
const REMOVED: u32 = 1000;
/// What is over is kept for days: it is looked for once in so many rounds, and
/// in the next round as well while a round left some behind.
const ROUNDS_TO_REMOVAL: u32 = 120;

pub(super) async fn run(state: Arc<App>) {
    let log = BackgroundLog::current();
    let (mut seen, mut rounds) = (RequestsSeen::default(), 0);
    loop {
        tokio::time::sleep(state.options.expiry_interval).await;
        let factory = (*state.factory).clone();
        let told = log.clone();
        let removing = rounds == 0;
        let mut passed = seen.clone();
        let ended = spawn_blocking(move || {
            let mut store = factory.open()?;
            let mut one =
                |repository: &str, error| told.failed("request expiry", repository, &error);
            store.expire_requests(BATCH, &mut one)?;
            if !removing {
                return Ok((passed, false));
            }
            let mut rule = |name: &str, error| told.failed("removal of what is over", name, &error);
            let removed = store.remove_what_is_over(REMOVED, &mut passed, &mut rule);
            Ok::<_, alt_server_store::Error>((passed, removed.more))
        });
        rounds = (rounds + 1) % ROUNDS_TO_REMOVAL;
        match ended.await {
            Ok(Ok((passed, more))) => {
                seen = passed;
                if more {
                    rounds = 0;
                }
            }
            Ok(Err(error)) => log.failed("request expiry", "", &error),
            Err(_) => {}
        }
    }
}
