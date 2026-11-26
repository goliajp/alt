//! What the refs of a view lead to, among the objects its segments hold.
//!
//! Asked of a repository whose segments hold what no ref names any more, each
//! time something is given or taken by an id alone. Finding it from nothing
//! reads every tree the refs reach, so what was found is kept with the tips it
//! was found for. A request whose tips lead to all of those goes on from what
//! was found for them and reads what is new: a push onto a branch, a branch
//! more, a branch less that another leads to. Only tips that lead to less than
//! the tips before them, as after a branch that nothing else leads to is
//! deleted or replaced, are walked from nothing again; and a walk that is
//! given up, or runs out of steps, leaves what it came to for the next.

use super::PackView;
use super::kept::{ASKED, keep, kept, loaded, store};
use crate::{Error, Result, Store};
use alt_pack_segment::{Led, Oid, SegmentSet};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// the repositories a walk is under way in, one in each
static WALKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ENDED: Condvar = Condvar::new();
/// Called with what the answers of a repository are kept under, where a walk
/// found that its refs lead to every object its segments hold and before that
/// is written down: a test moves the refs and the segments in between. None
/// is set outside tests.
#[doc(hidden)]
pub static FOUND_ALL: Mutex<Option<FoundAll>> = Mutex::new(None);
#[doc(hidden)]
pub type FoundAll = Arc<dyn Fn(&str) + Send + Sync>;

/// The one walk a repository has at a time: the others that ask wait for it
/// and go on from what it found.
struct Turn<'a>(&'a str);

impl<'a> Turn<'a> {
    fn taken(repository: &'a str, gone: Option<&AtomicBool>) -> Result<Self> {
        let mut walked = WALKED.lock().expect("walks");
        while walked.iter().any(|walked| walked == repository) {
            if gone.is_some_and(|gone| gone.load(Relaxed)) {
                return Err(Error::Busy("the request is waited for no longer"));
            }
            let waited = ENDED.wait_timeout(walked, Duration::from_millis(100));
            walked = waited.expect("walks").0;
        }
        walked.push(repository.to_owned());
        Ok(Turn(repository))
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut walked = WALKED.lock().expect("walks");
        walked.retain(|walked| walked != self.0);
        ENDED.notify_all();
    }
}

/// What a walk for `tips` goes on from: the latest kept whose tips `tips` lead
/// to, all of them, and whether that is whole. A tip that is among `tips` no
/// longer is led to where it is a commit one of them reaches, which the tips
/// that are new are asked first: after a push it is the commit below what was
/// pushed.
fn begun(
    repository: &str,
    segments: &SegmentSet,
    tips: &[Oid],
    steps: usize,
    gone: Option<&AtomicBool>,
    disk: Option<(Vec<Oid>, Arc<Led>)>,
) -> Result<Option<(Arc<Led>, bool)>> {
    let mut earlier: Vec<(Vec<Oid>, Arc<Led>, bool)> = {
        let asked = ASKED.lock().expect("led");
        let of_this = asked.iter().filter(|kept| kept.repository == repository);
        let all = of_this.map(|kept| (kept.tips.clone(), kept.led.clone(), kept.whole));
        all.rev().collect()
    };
    // what was read from disk and was too large to keep
    let disk = disk.filter(|(_, read)| !earlier.iter().any(|(_, led, _)| Arc::ptr_eq(led, read)));
    earlier.extend(disk.map(|(tips, led)| (tips, led, true)));
    let among = |tips: &[Oid], tip: &Oid| tips.binary_search(tip).is_ok();
    // each question of the commits takes `steps` of them at the most, and all
    // of them together no more than the segments hold objects: as long as one
    // walk of every commit there is
    let mut shared = usize::try_from(segments.objects()).unwrap_or(usize::MAX);
    for (theirs, led, whole) in earlier {
        let left = |tip: &&Oid| !among(tips, tip);
        let left: Vec<Oid> = theirs.iter().filter(left).copied().collect();
        let new = |tip: &&Oid| !among(&theirs, tip);
        let new: Vec<Oid> = tips.iter().filter(new).copied().collect();
        let mut lead = |from: &[Oid]| {
            let mut these = steps.min(shared);
            let asked = segments.lead_to_all_while(from, &left, &mut these, gone);
            shared -= steps.min(shared) - these;
            match asked {
                Ok(all) => Ok(all),
                // too far to tell: as if they did not
                Err(alt_pack_segment::Error::Limit(_)) => Ok(false),
                Err(alt_pack_segment::Error::GivenUp) => {
                    Err(Error::Busy("the request is waited for no longer"))
                }
                Err(_) => Err(Error::Integrity("objects the refs reach")),
            }
        };
        if left.is_empty() || lead(&new)? || lead(tips)? {
            return Ok(Some((led, whole)));
        }
    }
    Ok(None)
}

/// Every object `tips` lead to among those the segments of a repository hold,
/// and whether that is every object they hold, which is told once: by the walk
/// that goes on from nothing, or from what a walk left that ended early.
pub(crate) fn led(
    repository: &str,
    segments: &SegmentSet,
    mut tips: Vec<Oid>,
    steps: usize,
    gone: Option<&AtomicBool>,
) -> Result<(Arc<Led>, bool)> {
    tips.sort_unstable();
    tips.dedup();
    if let Some(led) = kept(repository, &tips) {
        return Ok((led, false));
    }
    let _turn = Turn::taken(repository, gone)?;
    let disk = loaded(repository);
    // the walk waited for, or one before the process began, may have found it
    let read = disk.as_ref().filter(|(read, _)| *read == tips);
    if let Some(led) = kept(repository, &tips).or_else(|| read.map(|(_, led)| led.clone())) {
        return Ok((led, false));
    }
    let unfit = |error| match error {
        alt_pack_segment::Error::Limit(_) => Error::Limit("objects the refs reach"),
        alt_pack_segment::Error::GivenUp => Error::Busy("the request is waited for no longer"),
        _ => Error::Integrity("objects the refs reach"),
    };
    let from = begun(repository, segments, &tips, steps, gone, disk)?;
    let from = from.map_or_else(Led::default, |(led, _)| (*led).clone());
    let had = from.len();
    let (led, ended) = segments.led_beyond(from, &tips, steps, gone);
    let led = Arc::new(led);
    match ended {
        Ok(()) => {
            // what was found is kept whether or not the objects it holds are
            // told; that it holds all of them is asked also of a walk that
            // went on from one that did, as a branch it reached may have gone
            store(repository, &tips, &led, had == 0);
            keep(repository, tips, led.clone(), true);
            let all = segments.all_led(&led).map_err(unfit)?;
            Ok((led, all))
        }
        Err(error) => {
            if led.len() > had {
                keep(repository, tips, led, false);
            }
            Err(unfit(error))
        }
    }
}

impl PackView {
    /// the tips of the refs, as the segments have ids
    fn tips(&self) -> Vec<Oid> {
        let tip = |oid: alt_git_codec::ObjectId| oid.as_bytes().try_into().expect("sha1 id");
        self.refs.values().map(tip).collect()
    }

    /// Every object the refs of a view that holds them all lead to. Reads
    /// `steps` trees at the most, and ends once `gone` is set.
    pub fn led(&self, steps: usize, gone: Option<&AtomicBool>) -> Result<Arc<Led>> {
        Ok(led(&self.led_under, &self.segments, self.tips(), steps, gone)?.0)
    }
}

impl Store {
    /// What the answers of a repository of this store are kept under: its id,
    /// with the directory of the stored bytes, which no other store of the
    /// process has.
    pub(crate) fn led_under(&self, repository: &str) -> String {
        format!("{}\0{repository}", self.bytes.root().display())
    }

    /// [`PackView::led`] for a view of this store. Where it finds that the
    /// refs lead to every object the segments hold, the repository is written
    /// down as one that holds nothing else, and is asked this no more until a
    /// branch of it is deleted or replaced again.
    pub fn led(&mut self, view: &PackView, gone: Option<&AtomicBool>) -> Result<Arc<Led>> {
        let repository = &view.scope.repository_instance_id;
        let tips = view.tips();
        let steps = self.limits.walk_steps;
        let (led, all) = led(&view.led_under, &view.segments, tips.clone(), steps, gone)?;
        if all {
            self.all_led(repository, tips, &view.segment_ids)?;
        }
        Ok(led)
    }

    /// Writes down that the segments of a repository hold nothing but what
    /// its refs lead to, where its refs are at `tips` and its live segments
    /// are `segments` still: whoever moves either holds the row of the
    /// repository, and none does while they are read. The refs alone could be
    /// back where they were with a segment more, which holds what a branch
    /// pushed and deleted in the meantime held.
    pub(crate) fn all_led(
        &mut self,
        repository: &str,
        mut tips: Vec<Oid>,
        segments: &[String],
    ) -> Result<()> {
        let found_all = FOUND_ALL.lock().expect("found all").clone();
        if let Some(found_all) = found_all {
            found_all(&self.led_under(repository));
        }
        tips.sort_unstable();
        tips.dedup();
        let tips: Vec<String> = tips.iter().map(alt_pack_segment::hex).collect();
        let mut tx = self.client.transaction()?;
        crate::store::instance(&mut tx, &self.spec, &self.signing_key)?;
        let held = "SELECT 1 FROM alt_repositories WHERE id=$1 AND pack_unreachable FOR UPDATE";
        if tx.query_opt(held, &[&repository])?.is_some() {
            let now = "SELECT coalesce(string_agg(DISTINCT oid COLLATE \"C\",' ' ORDER BY oid COLLATE \"C\"),'') FROM alt_refs WHERE repository=$1";
            let live = Self::live_segment_ids(&mut tx, repository)?;
            let same = live.iter().map(|(id, _)| id).eq(segments);
            if same && tx.query_one(now, &[&repository])?.get::<_, String>(0) == tips.join(" ") {
                let all = "UPDATE alt_repositories SET pack_unreachable=false WHERE id=$1";
                tx.execute(all, &[&repository])?;
            }
        }
        Ok(tx.commit()?)
    }
}
