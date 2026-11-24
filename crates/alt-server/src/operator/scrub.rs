//! What reading every object of the pack segments found, as the operator is told.

use super::Result;
use alt_json::{Value, json};
use alt_server_store::{Credential, Scrubbed, Store};

/// Reads the live segments of `repository`, or of every repository, and with
/// `mend` replaces those that do not give their objects back.
pub fn read(
    store: &mut Store,
    admin: &Credential,
    repository: Option<&str>,
    mend: bool,
) -> Result<Value> {
    let repositories = match repository {
        Some(repository) => vec![repository.to_owned()],
        None => store.repositories_to_scrub(admin)?,
    };
    let mut scrubbed = Vec::with_capacity(repositories.len());
    for repository in &repositories {
        scrubbed.push(store.scrub_packs(admin, repository, mend)?);
    }
    Ok(told(&scrubbed))
}

/// `unread_objects` are those no read gives back as the segments stand now:
/// those of segments that were not replaced.
fn told(scrubbed: &[Scrubbed]) -> Value {
    let (mut segments, mut objects, mut left) = (0, 0, 0);
    let mut repositories = Vec::new();
    for repository in scrubbed {
        segments += repository.segments;
        objects += repository.objects;
        let mut unread = Vec::new();
        for segment in &repository.unread {
            if segment.replaced_by.is_none() {
                left += segment.objects;
            }
            let named: Vec<Value> = (segment.named.iter())
                .map(|(object, reason)| json!({"object":object,"reason":reason}))
                .collect();
            unread.push(json!({"segment":segment.segment,"objects":segment.objects,"first":named,"replaced_by":segment.replaced_by,"kept_because":segment.kept_because}));
        }
        if !unread.is_empty() {
            repositories.push(json!({"repository":repository.repository,"segments":unread}));
        }
    }
    json!({"repositories":scrubbed.len(),"segments":segments,"objects":objects,"unread_objects":left,"unread":repositories})
}
