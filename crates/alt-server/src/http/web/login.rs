//! Password sign-in. The password check costs one Argon2 computation, and the
//! request may stop waiting during it; a session nobody receives is never opened.
use super::input::Login;
use super::*;
use alt_server_store::Store;
use std::sync::atomic::{AtomicU8, Ordering};

const UNDECIDED: u8 = 0;
const WRITING: u8 = 1;
const ABANDONED: u8 = 2;

/// Decides once whether the worker opens the session or the request gave up
/// first, so the response can say which of the two happened.
#[derive(Default)]
pub(super) struct Claim(AtomicU8);
impl Claim {
    fn decide(&self, outcome: u8) -> bool {
        self.0
            .compare_exchange(UNDECIDED, outcome, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    /// the request stopped waiting; false when the session write already began
    pub(super) fn abandon(&self) -> bool {
        self.decide(ABANDONED)
    }
}

pub(super) fn run(
    store: &mut Store,
    browser: &WebOptions,
    key: &alt_crypto::ed25519::SigningKey,
    peer: IpAddr,
    claim: &Claim,
    raw: &[u8],
) -> Result<(Output, Option<String>)> {
    let input: Login = alt_json::from_slice(raw)?;
    let proof = store
        .verify_browser_password(&browser.origin, &input.login_name, &input.password, peer)?
        .ok_or(Error::Unauthorized)?;
    if !claim.decide(WRITING) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "sign-in outlived its request",
        )
        .into());
    }
    let authenticated = store
        .open_browser_session(&browser.origin, proof, browser.session_ttl_ms)?
        .ok_or(Error::Unauthorized)?;
    let credential = store.authenticate(&authenticated.token)?;
    let session = execute::session(
        store,
        &credential,
        &browser.origin,
        &authenticated.token,
        key,
    )?;
    let cookie = browser
        .origin
        .set_cookie(&authenticated.token, browser.session_ttl_ms / 1000)
        .map_err(|_| Error::Integrity("session cookie"))?;
    Ok((Output::Json(session), Some(cookie)))
}

/// the response when the worker produced none in time; `unopened` is a sign-in
/// abandoned before its session write
pub(super) fn unfinished(writing: bool, unopened: bool) -> Response<ResponseBody> {
    if unopened {
        return failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Sign-in did not finish in time and no session was opened",
            None,
            Retry::SameRequest,
        );
    }
    failure(
        503,
        if writing {
            ErrorCode::OutcomeUnknown
        } else {
            ErrorCode::TemporarilyUnavailable
        },
        "The response is unavailable; inspect the current state before retrying a change",
        None,
        if writing {
            Retry::QueryOutcome
        } else {
            Retry::SameRequest
        },
    )
}
