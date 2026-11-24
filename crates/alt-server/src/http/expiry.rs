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
//!
//! And the same rounds give the requests that ended before schema 22 the moment
//! they ended, which the upgrade left to the server: so many in a round, until a
//! round finds none.

use super::*;
use alt_server_store::RequestsSeen;
use alt_server_telemetry::BackgroundLog;

/// the requests ended in one repository in one round
const BATCH: u32 = 1000;
/// the rows one rule removes in one round of removing what is over
const REMOVED: u32 = 1000;
/// the requests given their moment in one transaction
const GIVEN: u32 = 1000;
/// The transactions of one round that give requests their moment. The table is
/// vacuumed after them, so it grows by no more than the rows of one round.
const GIVINGS: u32 = 20;
/// What is over is kept for days: it is looked for once in so many rounds, and
/// in the next round as well while a round left some behind.
const ROUNDS_TO_REMOVAL: u32 = 120;

/// Which rounds remove what is over: the first, each one after a round that left
/// some behind, and otherwise one in [`ROUNDS_TO_REMOVAL`].
#[derive(Default)]
struct Rounds(u32);
impl Rounds {
    fn removes(&self) -> bool {
        self.0 == 0
    }
    /// The round is over; `more` where it removed and left some behind.
    fn over(&mut self, more: bool) {
        self.0 = if more {
            0
        } else {
            (self.0 + 1) % ROUNDS_TO_REMOVAL
        };
    }
}

pub(super) async fn run(state: Arc<App>) {
    let log = BackgroundLog::current();
    let (mut seen, mut rounds) = (RequestsSeen::default(), Rounds::default());
    // whether some request that is over may still be without its moment
    let mut owed = true;
    loop {
        tokio::time::sleep(state.options.expiry_interval).await;
        let factory = (*state.factory).clone();
        let told = log.clone();
        let removing = rounds.removes();
        let mut passed = seen.clone();
        let ended = spawn_blocking(move || {
            let mut store = factory.open()?;
            let mut one =
                |repository: &str, error| told.failed("request expiry", repository, &error);
            // the rest of the round does not wait for the requests to be ended
            if let Err(error) = store.expire_requests(BATCH, &mut one) {
                told.failed("request expiry", "", &error);
            }
            let owed = owed
                && match store.give_earlier_requests_their_moment(GIVEN, GIVINGS) {
                    Ok((_, more)) => more,
                    Err(error) => {
                        told.failed("moments of earlier requests", "", &error);
                        true
                    }
                };
            if !removing {
                return Ok((passed, false, owed));
            }
            let mut rule = |name: &str, error| told.failed("removal of what is over", name, &error);
            let removed = store.remove_what_is_over(REMOVED, &mut passed, &mut rule);
            Ok::<_, alt_server_store::Error>((passed, removed.more, owed))
        });
        match ended.await {
            Ok(Ok((passed, left, still))) => {
                (seen, owed) = (passed, still);
                rounds.over(left);
            }
            // a round that could not begin removed nothing: the next one is as it was
            Ok(Err(error)) => log.failed("request expiry", "", &error),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_over_is_removed_in_the_first_round_and_again_at_once_while_some_is_left() {
        let mut rounds = Rounds::default();
        // the first round after a start removes; it left some, and so does the next
        assert!(rounds.removes());
        rounds.over(true);
        assert!(rounds.removes());
        rounds.over(true);
        assert!(rounds.removes());
        // nothing left: not again until so many rounds have passed
        rounds.over(false);
        for round in 1..ROUNDS_TO_REMOVAL {
            assert!(!rounds.removes(), "round {round}");
            rounds.over(false);
        }
        assert!(rounds.removes());
    }
}
