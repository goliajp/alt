//! What the refs of a view lead to, among the objects its segments hold.
//!
//! Asked of a repository whose segments hold what no ref names any more, each
//! time something is given or taken by an id alone. Finding it reads every tree
//! the refs reach, so the answer is kept for the requests that ask it of the
//! same refs and the same segments.

use super::PackView;
use crate::{Error, Result};
use alt_pack_segment::{Led, SegmentSet};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

/// the answers kept, the latest last
const KEPT: usize = 8;

static ASKED: Mutex<Vec<([u8; 32], Arc<Led>)>> = Mutex::new(Vec::new());

impl PackView {
    /// Every object the refs of a view that holds them all lead to. Reads
    /// `steps` trees at the most, and ends once `gone` is set.
    pub fn led(&self, steps: usize, gone: Option<&AtomicBool>) -> Result<Arc<Led>> {
        let tips = (self.refs.values())
            .map(|oid| oid.as_bytes().try_into().expect("sha1 object id"))
            .collect();
        let ids = self.segment_ids.iter().map(String::as_str);
        led(
            (&self.scope.repository_instance_id, ids, &self.segments),
            tips,
            steps,
            gone,
        )
    }
}

/// Every object `tips` lead to among those the segments `ids` of a repository
/// hold, kept for whoever asks it of the same tips and segments.
pub(crate) fn led<'a>(
    (repository, ids, segments): (&str, impl Iterator<Item = &'a str>, &SegmentSet),
    mut tips: Vec<alt_pack_segment::Oid>,
    steps: usize,
    gone: Option<&AtomicBool>,
) -> Result<Arc<Led>> {
    tips.sort_unstable();
    tips.dedup();
    let mut asked = alt_hash::blake3::Hasher::new();
    asked.update(repository.as_bytes());
    for id in ids {
        asked.update(&[0]);
        asked.update(id.as_bytes());
    }
    asked.update(&[1]);
    for tip in &tips {
        asked.update(tip);
    }
    let asked = *asked.finalize().as_bytes();
    let kept = |kept: &Vec<([u8; 32], Arc<Led>)>| {
        let found = kept.iter().find(|(key, _)| *key == asked);
        found.map(|(_, led)| led.clone())
    };
    if let Some(led) = kept(&ASKED.lock().expect("led")) {
        return Ok(led);
    }
    let led = segments.led_to(&tips, steps, gone);
    let led = Arc::new(led.map_err(|error| match error {
        alt_pack_segment::Error::Limit(_) => Error::Limit("objects the refs reach"),
        alt_pack_segment::Error::GivenUp => Error::Busy("the request is waited for no longer"),
        _ => Error::Integrity("objects the refs reach"),
    })?);
    let mut all = ASKED.lock().expect("led");
    if kept(&all).is_none() {
        if all.len() == KEPT {
            all.remove(0);
        }
        all.push((asked, led.clone()));
    }
    Ok(led)
}
