//! A push as a server hands it over: its pack where it was received and
//! its commands as they were read.

use alt_wire::push::RefUpdate;

use super::{Landed, Push, Res, land, prepare_pack, signed};
use crate::held::Held;
use crate::native::{Principal, Store};

/// How the unpack went and, when the refs moved, what `moved` gave (none
/// for a push of no commands); otherwise the reason every command is
/// refused with.
pub type Report<T> = (Result<(), String>, Result<Option<T>, String>);

/// What a server does once a push is in its log, with the store still in
/// hand.
pub trait Recorded<T> {
    /// the refs moved
    fn moved(self) -> T;
    /// the push could not be made durable for the reason given
    fn unsynced(self, why: &str);
}

/// Takes in the push of `updates` by `principal` (and how its signature
/// went) with the pack at `pack`, when it sent one, what that pack may
/// hold and where the bases it leaves out are read. The pack is indexed
/// and the signatures of its commits are checked before the store is
/// taken in hand; `recorded` is called with it still in hand.
pub fn take_in<T>(
    store: &Held<Store>,
    pack: Option<(
        &std::path::Path,
        alt_git_pack::IndexLimits,
        alt_git_pack::Bases<'_>,
    )>,
    updates: &[RefUpdate],
    (principal, sig_label): &(Principal, &str),
    recorded: impl Recorded<T>,
) -> Res<Report<T>> {
    // a pack that fails to index is reported as the unpack error
    let prepared = match pack.map(|(path, limits, bases)| prepare_pack(path, &limits, bases)) {
        Some(Err(e)) => {
            let unpack = Err(format!("index-pack: {e}"));
            return Ok((unpack, Err("pack unpack failed".into())));
        }
        Some(Ok(prepared)) => Some(prepared),
        None => None,
    };
    // nothing would name the objects
    if updates.is_empty() {
        return Ok((Ok(()), Ok(None)));
    }
    let trust = signed::trust_for(&store.take(), principal)?;
    let checked = match trust {
        Some(trust) => Some(signed::check_pack(trust, prepared.as_ref())?),
        None => None,
    };
    let push = Push {
        pack: prepared.as_ref(),
        checked: checked.as_ref(),
        updates,
        principal,
        sig_label,
    };
    let mut store = store.take();
    Ok(match land(&mut store, &push)? {
        Landed::Moved => (Ok(()), Ok(Some(recorded.moved()))),
        Landed::Unpack(e) => (Err(e), Err("pack unpack failed".into())),
        Landed::Refused(reason) => (Ok(()), Err(reason)),
        Landed::Unsynced(why) => {
            recorded.unsynced(&why);
            (Ok(()), Err(format!("durability: {why}")))
        }
    })
}
