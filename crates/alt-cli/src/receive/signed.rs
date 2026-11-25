//! The `require-signed-commits` policy of a push.
//!
//! It holds for every commit the push adds to the history served: one a
//! command's new value reaches and no ref reached before the push. Such a
//! commit need not come in the push's pack; a push without objects that
//! points a ref at a commit lying in the odb unreferenced adds it just the
//! same. A commit refs already reach was let in under the policy of
//! whoever pushed it, and is not looked at again, in the pack or not: a
//! client that lacks a commit the server advertises sends what lies
//! under it once more.
//!
//! The signature of every commit of the pack is checked before the store
//! is taken in hand: that needs the commit's bytes and the trust keys,
//! nothing the store guards. Whether a commit that fails is one the push
//! adds is told by the walk over what it adds (see
//! [`crate::added_commits`]), where the commits it adds from outside the
//! pack are checked as well.

use alt_git_codec::{ObjectId, ObjectKind};
use alt_hash::KeyedIdMap;

use super::{Objects, PreparedPack};
use crate::native::{Principal, Store};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// What the policy is checked with, and of each commit of the pack why
/// it fails the policy, when it does.
pub struct Checked {
    trust: Vec<(String, alt_sign::PublicKey)>,
    pack: KeyedIdMap<ObjectId, Option<String>>,
}

/// Why the commit `oid` does not satisfy the policy, when it does not.
fn refusal(
    trust: &[(String, alt_sign::PublicKey)],
    oid: &ObjectId,
    bytes: &[u8],
) -> Option<String> {
    let parsed = match crate::commit_sign::extract_alt_sig(bytes) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            return Some(format!(
                "commit {oid} missing alt-sig (require-signed-commits)"
            ));
        }
        Err(e) => return Some(format!("commit {oid} alt-sig unreadable: {e}")),
    };
    let Some((_, pubkey)) = trust.iter().find(|(id, _)| id == &parsed.principal) else {
        return Some(format!(
            "commit {oid} signed by '{}' not in trust store",
            parsed.principal
        ));
    };
    pubkey
        .verify(&parsed.canonical, &parsed.sig)
        .is_err()
        .then(|| {
            format!(
                "commit {oid} alt-sig did not verify against trust['{}']",
                parsed.principal
            )
        })
}

/// The trust keys, when the policy of `principal` asks for signed
/// commits; `None` when it does not.
pub fn trust_for(
    store: &Store,
    principal: &Principal,
) -> Res<Option<Vec<(String, alt_sign::PublicKey)>>> {
    if !store.capabilities_for(principal).require_signed_commits {
        return Ok(None);
    }
    store.trust_keys().map(Some)
}

/// Checks the commits of `prepared` against `trust`, while `gone` is not
/// set. Nothing of the store is needed.
pub fn check_pack(
    trust: Vec<(String, alt_sign::PublicKey)>,
    prepared: Option<&PreparedPack>,
    gone: Option<&std::sync::atomic::AtomicBool>,
) -> Res<Checked> {
    let mut pack = KeyedIdMap::default();
    if let Some((ip, order)) = prepared {
        for &(offset, i) in order {
            super::gone_on(gone)?;
            let obj = ip.read_at(offset)?;
            if obj.kind == ObjectKind::Commit {
                let oid = ip.idx().oid_at(i);
                pack.insert(oid, refusal(&trust, &oid, &obj.data));
            }
        }
    }
    Ok(Checked { trust, pack })
}

impl Checked {
    /// Why the commit `oid` fails the policy, when it does.
    pub(super) fn fails(&self, objects: &Objects<'_>, oid: &ObjectId) -> Res<Option<String>> {
        if let Some(known) = self.pack.get(oid) {
            return Ok(known.clone());
        }
        let held = objects.get(oid)?;
        Ok(held.and_then(|raw| refusal(&self.trust, oid, &raw.data)))
    }
}
