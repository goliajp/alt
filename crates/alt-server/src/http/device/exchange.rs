//! A code exchanged for the token it stands for, and the token taken back where
//! nobody is given it.
//!
//! The token is good from the moment the account consents; the exchange only
//! hands it over, and the code is gone with that. A token that was not handed
//! over is one nobody has, good for hours and counted among those its account
//! may have. So the request and the worker that exchanges for it agree, once,
//! on who is still there: a worker whose request left before the code is spent
//! leaves the code as it was, and one whose request left afterwards revokes
//! the token.
use super::*;
use std::sync::atomic::{AtomicU8, Ordering};

const WAITED_FOR: u8 = 0;
const SPENDING: u8 = 1;
const UNSPENT: u8 = 2;
const LEFT: u8 = 3;
const HANDED_OVER: u8 = 4;

/// what a request that waits no longer finds of its exchange
#[derive(Debug, PartialEq)]
pub(super) enum Leaving {
    /// the code is as it was
    Unspent,
    /// the code is spent or about to be, and its token is taken back
    Withdrawn,
    /// the answer is made and only has to be taken
    Answered,
}

#[derive(Default)]
pub(super) struct Handover(AtomicU8);

impl Handover {
    fn moved(&self, from: u8, to: u8) -> bool {
        self.0
            .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    /// the worker is about to spend the code; false where the request has left
    fn spend(&self) -> bool {
        self.moved(WAITED_FOR, SPENDING)
    }
    /// the worker has the answer; false where the request left meanwhile
    fn hand_over(&self) -> bool {
        self.moved(SPENDING, HANDED_OVER)
    }
    /// the request waits no longer
    pub(super) fn leave(&self) -> Leaving {
        if self.moved(WAITED_FOR, UNSPENT) {
            Leaving::Unspent
        } else if self.moved(SPENDING, LEFT) {
            Leaving::Withdrawn
        } else if self.0.load(Ordering::Acquire) == HANDED_OVER {
            Leaving::Answered
        } else {
            Leaving::Unspent
        }
    }
}

/// leaves the exchange when the request that waits for it is let go of
pub(super) struct Waiting(pub(super) Arc<Handover>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// Revokes the token of a code that was spent for nobody. On a connection of its
/// own: the one the exchange ran on may be what failed.
fn withdraw(factory: &StoreFactory, token: &str) -> alt_server_store::Result<()> {
    factory.open()?.revoke_client_token(token)
}

/// What the device that polls with the code of `digest` is answered.
pub(super) fn exchange(
    factory: &StoreFactory,
    store: &mut Store,
    digest: &str,
    handover: &Handover,
) -> Result<alt_json::Value, Refused> {
    let token = credential(&factory.signing_key, digest);
    let expires = match store.poll_device_authorization(digest, || handover.spend()) {
        Ok(DevicePoll::Waiting(error)) => return Err(error.into()),
        Ok(DevicePoll::Ready { expires_at_unix_ms }) => expires_at_unix_ms,
        // the code may be gone or not: with its token revoked it is good for
        // nothing either way, and the device is told to begin again
        Err(Error::OutcomeUnknown) => {
            withdraw(factory, &token).map_err(mapped)?;
            return Err(DeviceError::ExpiredToken.into());
        }
        Err(error) => return Err(mapped(error)),
    };
    // the code is spent from here on: asking again with it gets nothing, so a
    // failure says to begin again, not to wait
    let answer = (|| {
        factory
            .authenticate(store, &token)
            .map_err(|error| match error {
                Error::Unauthorized => DeviceError::AccessDenied,
                _ => DeviceError::ExpiredToken,
            })?;
        let remaining = expires.saturating_sub(crate::cloud::clock()) / 1000;
        if remaining == 0 {
            return Err(DeviceError::ExpiredToken);
        }
        alt_json::to_value(DeviceTokenResponse {
            access_token: token.clone(),
            token_type: "Bearer".into(),
            expires_in: remaining,
            scope: CLI_SCOPE.into(),
        })
        .map_err(|_| DeviceError::ExpiredToken)
    })();
    let answer = match answer {
        Ok(_) if !handover.hand_over() => Err(DeviceError::ExpiredToken),
        answer => answer,
    };
    if answer.is_err() {
        // the device is told the same whether or not the token could be
        // revoked: the code is spent whichever it is
        if let Err(error) = withdraw(factory, &token) {
            alt_server_telemetry::BackgroundLog::current().failed("device token", "", &error);
        }
    }
    answer.map_err(Refused::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_and_its_worker_agree_once_on_who_is_still_there() {
        // the request leaves first: the worker spends nothing
        let handover = Handover::default();
        assert_eq!(handover.leave(), Leaving::Unspent);
        assert!(!handover.spend());
        assert_eq!(handover.leave(), Leaving::Unspent);

        // the request leaves while the code is being spent: the worker finds
        // nobody to hand the token over to
        let handover = Handover::default();
        assert!(handover.spend());
        assert_eq!(handover.leave(), Leaving::Withdrawn);
        assert!(!handover.hand_over());

        // the worker is done first: the request has its answer however late it looks
        let handover = Handover::default();
        assert!(handover.spend());
        assert!(handover.hand_over());
        assert_eq!(handover.leave(), Leaving::Answered);

        // a request let go of leaves as one that waits no longer does
        let handover = Arc::new(Handover::default());
        assert!(handover.spend());
        drop(Waiting(handover.clone()));
        assert!(!handover.hand_over());
    }
}
