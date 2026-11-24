//! The signature a push carries where signing is on.

use super::{NativeRepo, Res, SignPolicy};

impl NativeRepo<'_> {
    /// The principal that signs and its signature of this push, for the
    /// capability list; nothing where signing is off or the principal has no
    /// key here, and the push then goes out unsigned.
    ///
    /// `certified` is the nonce the server gave for this push and the
    /// repository pushed to, where the server takes signatures over a
    /// certificate of them. A server that does not is one from before
    /// certificates: it is signed for as it verifies, over the nonce where it
    /// gave one, and the updates.
    pub(super) fn maybe_sign_push(
        &self,
        updates: &[alt_wire::RefUpdate],
        nonce: Option<&str>,
        certified: Option<(&str, &str)>,
    ) -> Res<Option<(String, String)>> {
        let policy = SignPolicy::load(&self.store.alt_dir)?;
        if !policy.enabled {
            return Ok(None);
        }
        let principal = policy
            .principal
            .unwrap_or_else(|| self.id.principal.id.clone());
        let sec_path = self
            .store
            .alt_dir
            .join("identity")
            .join(format!("{principal}.sec"));
        let sec_text = match std::fs::read_to_string(&sec_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let sec = alt_sign::SecretKey::from_text(&sec_text)
            .map_err(|e| format!("malformed sec key at {}: {e}", sec_path.display()))?;
        let algo = self.store.algo()?;
        let payload = match certified {
            Some((nonce, pushee)) => {
                alt_wire::push_certificate(&principal, pushee, nonce, updates, algo)
            }
            None => alt_wire::canonical_push_payload_with_nonce(updates, nonce, algo),
        };
        // the text of a signature ends in a newline, which a capability cannot carry
        let sig_text = sec.sign(&payload).to_text().trim().to_owned();
        Ok(Some((principal, sig_text)))
    }
}
