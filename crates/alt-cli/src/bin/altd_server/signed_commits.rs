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
//! lock is taken: that needs the commit's bytes and the trust keys,
//! nothing the lock guards. Whether a commit that fails is one the push
//! adds is told under the lock (see [`alt_cli::added_commits`]), where the
//! commits it adds from outside the pack are checked as well.

use std::collections::HashMap;
use std::sync::Mutex;

use alt_cli::added_commits::Source;
use alt_cli::native::{Principal, Store};
use alt_git_codec::{ObjectId, ObjectKind};

use super::pack_ingest::{Objects, PreparedPack};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// What the policy is checked with, and of each commit of the pack why
/// it fails the policy, when it does.
pub(super) struct Checked {
    trust: Vec<(String, alt_sign::PublicKey)>,
    pack: HashMap<ObjectId, Option<String>>,
}

/// Why the commit `oid` does not satisfy the policy, when it does not.
fn refusal(
    trust: &[(String, alt_sign::PublicKey)],
    oid: &ObjectId,
    bytes: &[u8],
) -> Option<String> {
    let parsed = match alt_cli::commit_sign::extract_alt_sig(bytes) {
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

/// Checks the commits of `prepared` when the policy of `principal` asks
/// for signed commits; `None` when it does not. The store lock is held
/// only to read the policy and the trust keys.
pub(super) fn check_pack(
    store: &Mutex<Store>,
    principal: &Principal,
    prepared: Option<&PreparedPack>,
) -> Res<Option<Checked>> {
    let trust = {
        let guard = store.lock().unwrap();
        if !guard.capabilities_for(principal).require_signed_commits {
            return Ok(None);
        }
        guard.trust_keys()?
    };
    let mut pack = HashMap::new();
    if let Some((ip, order)) = prepared {
        for &(offset, i) in order {
            let obj = ip.read_at(offset)?;
            if obj.kind == ObjectKind::Commit {
                let oid = ip.idx().oid_at(i);
                pack.insert(oid, refusal(&trust, &oid, &obj.data));
            }
        }
    }
    Ok(Some(Checked { trust, pack }))
}

impl Checked {
    /// The reason to refuse a push that points refs at `new`, where the
    /// refs point at `served`, when it adds a commit to the history that
    /// fails the policy.
    pub(super) fn added_refusal(
        &self,
        objects: &Objects<'_>,
        served: &[ObjectId],
        new: &[ObjectId],
    ) -> Res<Option<String>> {
        let get = |oid: &ObjectId| objects.get(oid);
        let fails = |oid: &ObjectId| -> Res<Option<String>> {
            if let Some(known) = self.pack.get(oid) {
                return Ok(known.clone());
            }
            let held = objects.get(oid)?;
            Ok(held.and_then(|raw| refusal(&self.trust, oid, &raw.data)))
        };
        let source = Source {
            get: &get,
            fails: &fails,
        };
        alt_cli::added_commits::refusal(&source, served, new)
    }
}
