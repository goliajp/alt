//! What reading every object of the pack segments found, as the operator is told.

use super::{Error, Result};
use alt_json::{Value, json};
use alt_pack_segment::Memory;
use alt_server_store::{Credential, Scrubbed, Store};

/// Reads the live segments of `repository`, or of every repository, and with
/// `mend` replaces those that do not give their objects back, their packs
/// indexed anew in that memory. A repository that could not be read through
/// does not keep the others from being read: what was found of them is told,
/// with the repositories that failed, and the command fails.
pub fn read(
    store: &mut Store,
    admin: &Credential,
    repository: Option<&str>,
    mend: Option<&'static Memory>,
) -> Result<Value> {
    let repositories = match repository {
        Some(repository) => vec![repository.to_owned()],
        None => store.repositories_to_scrub(admin)?,
    };
    let mut scrubbed = Vec::with_capacity(repositories.len());
    let (mut failed, mut unknown) = (Vec::new(), false);
    for repository in &repositories {
        match store.scrub_packs(admin, repository, mend) {
            Ok(found) => scrubbed.push(found),
            Err(error) => {
                unknown |= matches!(error, alt_server_store::Error::OutcomeUnknown);
                failed.push(json!({"repository":repository,"error":error.to_string()}));
            }
        }
    }
    let mut told = told(&scrubbed);
    if failed.is_empty() {
        return Ok(told);
    }
    told["failed"] = Value::Array(failed);
    Err(Error::Partly(told, unknown))
}

/// The memory the packs of segments are indexed anew in: what the service
/// verifies the packs of pushes in, all of it for one pack at a time.
pub fn memory(bytes: u64) -> &'static Memory {
    Box::leak(Box::new(Memory::new(bytes)))
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
