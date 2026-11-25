//! Which of the Git objects its segments hold a repository gives to a publication.
//!
//! The segments of a repository whose history was replaced, or that had a branch
//! deleted, hold objects no ref leads to. A publication is given what the refs
//! lead to and nothing else: an object beside that it brings itself, as it
//! brings one the repository never had.
use crate::database::Transaction;
use crate::{Error, Result, Store};
use alt_pack_segment::{Led, Oid, SegmentSet};
use alt_remote_protocol::ContentEntry;
use std::sync::{Arc, OnceLock};

pub(crate) struct Reached {
    /// as [`Store::led_under`] has it
    repository: String,
    /// none where every object of the segments is led to
    tips: Option<Vec<Oid>>,
    steps: usize,
    /// with whether it was found to be every object of the segments
    led: OnceLock<(Arc<Led>, bool)>,
}

impl Reached {
    fn led(&self, segments: &SegmentSet) -> Result<Option<&Led>> {
        let Some(tips) = &self.tips else {
            return Ok(None);
        };
        if self.led.get().is_none() {
            let repository = self.repository.as_str();
            let led = crate::pack_view::led(repository, segments, tips.clone(), self.steps, None)?;
            let _ = self.led.set(led);
        }
        Ok(self.led.get().map(|(led, _)| &**led))
    }
    /// the tips, where they were found to lead to every object of the segments
    fn all(&self) -> Option<Vec<Oid>> {
        let all = self.led.get().is_some_and(|(_, all)| *all);
        self.tips.clone().filter(|_| all)
    }
    /// whether the refs lead to an object `segments` hold
    pub(crate) fn to(&self, segments: &SegmentSet, oid: &Oid) -> Result<bool> {
        Ok(self.led(segments)?.is_none_or(|led| led.to(oid)))
    }
}

const LISTED: &str = "SELECT r.pack_unreachable,(SELECT string_agg(f.oid||' '||f.name,E'\\n') FROM alt_refs f WHERE f.repository=r.id AND r.pack_unreachable),(SELECT coalesce(jsonb_agg(jsonb_build_array(s.segment_id,s.inline IS NOT NULL) ORDER BY s.sequence,s.segment_id),'[]'::jsonb) FROM alt_pack_segments s WHERE s.repository=r.id AND s.state='live') FROM alt_repositories r WHERE r.id=$1";

/// what a transaction found of a repository: its live segments, and the tips
/// of its refs where the segments hold what no ref leads to
pub(crate) struct Listed {
    pub(crate) ids: Vec<(String, bool)>,
    tips: Option<Vec<Oid>>,
}

impl Listed {
    /// The segments and the tips as one statement sees them: a list and tips
    /// of two moments could name a tip the segments do not hold yet.
    pub(crate) fn of(tx: &mut Transaction<'_>, repository: &str) -> Result<Self> {
        let Some(row) = tx.query_opt(LISTED, &[&repository])? else {
            return Ok(Self {
                ids: Vec::new(),
                tips: None,
            });
        };
        let tips = if row.get::<_, bool>(0) {
            let refs = crate::refs::map(row.get::<_, Option<&str>>(1))?;
            let tip = |oid: &alt_git_codec::ObjectId| {
                Oid::try_from(oid.as_bytes()).map_err(|_| Error::Integrity("stored reference"))
            };
            Some(refs.values().map(tip).collect::<Result<_>>()?)
        } else {
            None
        };
        Ok(Self {
            ids: row.get::<_, crate::Jsonb>(2).read()?,
            tips,
        })
    }
}

/// the live segments of a repository, and what they give to a publication
pub(crate) struct Published {
    pub(crate) segments: SegmentSet,
    pub(crate) reached: Reached,
}

impl Published {
    /// Whether the repository gives the Git object this manifest entry names:
    /// the segments hold it as the entry says, and the refs lead to it.
    pub(crate) fn holds_entry(&self, entry: &ContentEntry) -> Result<bool> {
        let key = entry.git_oid.as_deref();
        let Some(key) = key.and_then(crate::pack_native::segment_key) else {
            return Ok(false);
        };
        Ok(crate::pack_native::holds_entry(&self.segments, entry)?
            && self.reached.to(&self.segments, &key)?)
    }
}

impl Store {
    /// the live segments of a repository as of now, for a caller that has yet to
    /// authorize whatever it decides from them
    pub(crate) fn published(&mut self, repository: &str) -> Result<Published> {
        let mut tx = self.client.transaction()?;
        let listed = Listed::of(&mut tx, repository)?;
        tx.commit()?;
        self.published_as(repository, listed)
    }
    /// The same with what the refs lead to read already, so that nothing asked
    /// of it reads that inside a transaction.
    pub(crate) fn published_walked(&mut self, repository: &str) -> Result<Published> {
        let published = self.published(repository)?;
        self.walked(repository, published)
    }
    pub(crate) fn published_walked_as(
        &mut self,
        repository: &str,
        listed: Listed,
    ) -> Result<Published> {
        let published = self.published_as(repository, listed)?;
        self.walked(repository, published)
    }
    fn walked(&mut self, repository: &str, published: Published) -> Result<Published> {
        published.reached.led(&published.segments)?;
        if let Some(tips) = published.reached.all() {
            self.all_led(repository, tips)?;
        }
        Ok(published)
    }
    fn published_as(&mut self, repository: &str, listed: Listed) -> Result<Published> {
        let (_, segments) = self.open_listed_segments(repository, listed.ids)?;
        Ok(Published {
            segments,
            reached: Reached {
                repository: self.led_under(repository),
                tips: listed.tips,
                steps: self.limits.walk_steps,
                led: OnceLock::new(),
            },
        })
    }
}
