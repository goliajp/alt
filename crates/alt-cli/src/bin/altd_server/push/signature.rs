//! Who a push is of: the signature it carries, and the policy that asks for one.

use super::*;

/// The principal of a push, how it signed, and the one it signed in as
/// where that is another.
pub(super) type Picked = (Principal, &'static str, Option<Principal>);

/// Who the push is of, and why it is refused for its signature where it is.
/// The signature is over the commands of the push and not its pack, so this
/// is decided before the pack is looked at.
pub(super) fn signed(
    writer: &WriteCoordinator,
    head: &alt_wire::push::PushHead,
    req: &http::Request,
    auth_user: Option<&str>,
) -> Result<(Picked, Option<String>), Box<dyn std::error::Error>> {
    let host = req.headers().iter().find(|h| h.field.equiv("Host"));
    let asked = (host.map(|h| h.value.as_str()), req.url());
    let sig_outcome = verify_push_signature(writer, head, asked)?;
    let principal = pick_principal(&sig_outcome, auth_user);
    let block = match &sig_outcome {
        SigOutcome::Verified { .. } => None,
        other if require_signed_for(&writer.store, &principal.0) => {
            Some(format!("signature required: {}", describe_sig(other)))
        }
        _ => None,
    };
    Ok((principal, block))
}

/// result of verifying the `alt-principal=<id>` +
/// `alt-sig=<ed25519>` capabilities the client may have attached to a
/// push. `NoSignature` is the empty-attribution baseline; the rest are
/// failure modes the policy gate can act on.
#[derive(Debug)]
pub(super) enum SigOutcome {
    Verified { principal_id: String },
    NoSignature,
    BadSignature(String),
    UnknownPrincipal(String),
}

pub(super) fn describe_sig(o: &SigOutcome) -> String {
    match o {
        SigOutcome::Verified { .. } => "verified".into(),
        SigOutcome::NoSignature => "no signature attached".into(),
        SigOutcome::BadSignature(e) => format!("signature did not verify ({e})"),
        SigOutcome::UnknownPrincipal(id) => format!("principal '{id}' not in trust store"),
    }
}

/// Run the signature check against `<alt-dir>/trust/`. Returns
/// `NoSignature` when the client didn't attach the pair (the common
/// path on git-native clients).
///
/// A signature is over a certificate that names the nonce this repository
/// gave for one push and the repository itself: at one of the addresses the
/// server is told it is reached at, or where it is told none, as the request
/// `asked` for it by its `Host` header; and by the path of the request. One
/// without them, or with a nonce that was used, or for another repository,
/// is a signature that could be sent again, here or elsewhere: it does not
/// verify. A server that goes by the `Host` header takes what was signed
/// for another server from whoever sends it on under that server's name.
pub(super) fn verify_push_signature(
    writer: &WriteCoordinator,
    head: &alt_wire::push::PushHead,
    asked: (Option<&str>, &str),
) -> Result<SigOutcome, Box<dyn std::error::Error>> {
    let cap = |name: &str| {
        let prefix = format!("{name}=");
        (head.capabilities.iter()).find_map(|c| c.strip_prefix(&prefix))
    };
    let (principal_id, sig_text) =
        match (cap(alt_wire::CAP_ALT_PRINCIPAL), cap(alt_wire::CAP_ALT_SIG)) {
            (Some(p), Some(s)) => (p.to_owned(), s.to_owned()),
            _ => return Ok(SigOutcome::NoSignature),
        };
    let trust = {
        let guard = writer.store.take();
        guard.trust_keys()?
    };
    let Some((_, pubkey)) = trust.iter().find(|(id, _)| id == &principal_id) else {
        return Ok(SigOutcome::UnknownPrincipal(principal_id));
    };
    let sig = match alt_sign::Sig::from_text(&sig_text) {
        Ok(s) => s,
        Err(e) => return Ok(SigOutcome::BadSignature(format!("{e}"))),
    };
    let bad = |why: &str| Ok(SigOutcome::BadSignature(why.into()));
    let (Some(nonce), Some(pushee)) = (cap(alt_wire::CAP_ALT_NONCE), cap(alt_wire::CAP_ALT_PUSHEE))
    else {
        return bad("the signature names no nonce of this server and no repository");
    };
    // against what the server is told it is reached at; where it is told
    // nothing, against the name its client gave it
    let here = &super::told::told().origins;
    let this = match here.is_empty() {
        true => (asked.0).is_some_and(|host| alt_wire::pushee_is(pushee, host, asked.1)),
        false => alt_wire::pushee_is_at(pushee, here, asked.1),
    };
    if !this {
        return bad("the signature is for another repository than this one");
    }
    let inactive = "alt-nonce echoed in push not active on this server (replay or expired)";
    let Some(given) = writer.nonces.good(nonce) else {
        return bad(inactive);
    };
    let payload =
        alt_wire::push_certificate(&principal_id, pushee, nonce, &head.updates, HashAlgo::Sha1);
    match pubkey.verify(&payload, &sig) {
        // used from here on: by this push, unless another had it meanwhile
        Ok(()) if writer.nonces.spend(given) => Ok(SigOutcome::Verified { principal_id }),
        Ok(()) => bad(inactive),
        Err(e) => Ok(SigOutcome::BadSignature(format!("{e}"))),
    }
}

/// Pick the effective principal + signature label for the op-log actor
/// string. A verified signature wins over Basic-auth (it's a stronger
/// claim of identity); otherwise we fall back to the Basic-auth user
/// (or "anonymous") and the label records *why* it isn't verified.
///
/// Where a verified signature names another principal than the request
/// signed in as, that one is returned beside it: the push is held to the
/// policy of both. A key does not take off what an account is held to.
pub(super) fn pick_principal(outcome: &SigOutcome, auth_user: Option<&str>) -> Picked {
    let (id, label) = match outcome {
        SigOutcome::Verified { principal_id } => (principal_id.clone(), "ed25519"),
        SigOutcome::NoSignature => (auth_user.unwrap_or("anonymous").to_owned(), "none"),
        SigOutcome::BadSignature(_) => (auth_user.unwrap_or("anonymous").to_owned(), "bad"),
        SigOutcome::UnknownPrincipal(_) => (
            auth_user.unwrap_or("anonymous").to_owned(),
            "unknown-principal",
        ),
    };
    let human = |id: String| Principal {
        kind: PrincipalKind::Human,
        id,
        session: None,
    };
    let signed_in = match (outcome, auth_user) {
        (SigOutcome::Verified { .. }, Some(user)) if user != id => Some(human(user.to_owned())),
        _ => None,
    };
    (human(id), label, signed_in)
}

/// Does this principal's policy require a verified signature on every
/// push? If so, an unverified push is short-circuited with `ng
/// signature required` before any objects land.
pub(super) fn require_signed_for(store: &Held<Store>, principal: &Principal) -> bool {
    let guard = store.take();
    guard.capabilities_for(principal).require_signed
}
