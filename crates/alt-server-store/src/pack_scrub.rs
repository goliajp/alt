//! Reading every object of a repository's live segments, and writing again the
//! index of a segment that does not give its objects back.
//!
//! A segment is verified once, when it comes in, and trusted from then on. This
//! is the check an operator asks for afterwards: each object is read the way a
//! fetch reads it and held against its id. A segment that fails is replaced, where
//! that is asked for, by one whose index is made anew of the same pack, the way a
//! merge replaces segments; where its pack does not verify either it stays as it
//! is and is told of.

use crate::{
    Credential, Error, Result, Store,
    packs::INLINE_SEGMENT_BYTES,
    store::{instance_unlocked, require_admin},
};
use alt_pack_segment::{Memory, Segment, SegmentSet};
use std::sync::Arc;

mod mend;

/// the objects told of by name for one segment; the rest are counted
const NAMED: usize = 16;

/// why a segment whose pack takes more memory to index than there is stays
const NO_MEMORY: &str = "memory to index the pack of the segment anew";

/// what a scrub of a name that is no repository is refused with
pub const NO_REPOSITORY: &str = "no repository of this name";

/// A live segment that does not give back every object of its index.
pub struct Unread {
    pub segment: String,
    /// how many of its objects are not read
    pub objects: u64,
    /// the first of them, each with what its read ended in
    pub named: Vec<(String, String)>,
    /// the segment that took its place, where one did
    pub replaced_by: Option<String>,
    /// why none did, where that was asked for
    pub kept_because: Option<String>,
}

impl Unread {
    fn of(segment: &str, lost: &[(alt_pack_segment::Oid, alt_pack_segment::Error)]) -> Self {
        Unread {
            segment: segment.to_owned(),
            objects: lost.len() as u64,
            named: (lost.iter().take(NAMED))
                .map(|(oid, error)| (alt_pack_segment::hex(oid), error.to_string()))
                .collect(),
            replaced_by: None,
            kept_because: None,
        }
    }
}

/// Segments whose objects were read, each by its repository and its id.
pub type Read = std::collections::BTreeSet<(String, String)>;

/// How far a reading of every object has come, told as each object was read:
/// the repository, how many of its objects were read, and how many it has.
pub type Reading<'a> = &'a mut dyn FnMut(&str, u64, u64);

/// What reading the live segments of one repository found.
pub struct Scrubbed {
    pub repository: String,
    pub segments: u64,
    pub objects: u64,
    pub unread: Vec<Unread>,
}

impl Store {
    /// Writes `content` as the file of a segment that has no row yet, under an id
    /// of its own. One small enough to be held by its row is not flushed.
    pub(crate) fn write_segment(
        &self,
        repository: &str,
        content: &[u8],
    ) -> Result<(String, std::path::PathBuf, crate::Writing)> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut nonce = [0u8; 16];
        alt_os::random::fill(&mut nonce);
        let id: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = self.pack_path(repository, &id)?;
        let writing = crate::writing(&path);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(content)?;
        if content.len() > INLINE_SEGMENT_BYTES {
            file.sync_all()?;
            std::fs::File::open(path.parent().expect("segment directory"))?.sync_all()?;
        }
        Ok((id, path, writing))
    }

    /// The repositories of the instance, in the order of their names.
    pub fn repositories_to_scrub(&mut self, admin: &Credential) -> Result<Vec<String>> {
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let rows = tx.query("SELECT id FROM alt_repositories ORDER BY id", &[])?;
        tx.commit()?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// Reads every object of the live segments of `repository`, oldest segment
    /// first. With `mend`, a segment that does not give back all of its objects is
    /// replaced by its pack indexed anew in that memory, and the segments behind
    /// it are read against what replaced it. A name that is no repository of the
    /// instance is an error: it has no segments, and would be told of as sound.
    pub fn scrub_packs(
        &mut self,
        admin: &Credential,
        repository: &str,
        mend: Option<&'static Memory>,
        told: Reading<'_>,
    ) -> Result<Scrubbed> {
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        require_admin(&mut tx, &self.spec, admin)?;
        let known = tx.query_opt("SELECT 1 FROM alt_repositories WHERE id=$1", &[&repository])?;
        if known.is_none() {
            return Err(Error::Precondition(NO_REPOSITORY));
        }
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit()?;
        let set = self.open_segments(repository, &ids)?;
        let mut scrubbed = Scrubbed {
            repository: repository.to_owned(),
            segments: ids.len() as u64,
            objects: set.objects(),
            unread: Vec::new(),
        };
        let mut older: Vec<Arc<Segment>> = Vec::with_capacity(ids.len());
        // the ids of those segments, the ones put in the place of others among them
        let mut older_ids: Vec<(String, bool)> = Vec::with_capacity(ids.len());
        let mut read = 0;
        let mut each = || {
            read += 1;
            told(repository, read, scrubbed.objects)
        };
        for (part, segment) in ids.iter().zip(set.segments()) {
            let before = SegmentSet::new(older.clone());
            let lost = alt_pack_segment::unread_telling(segment, &before, &mut each)
                .map_err(|_| Error::Integrity("pack segment index"))?;
            let mut stands = (part.clone(), Arc::clone(segment));
            if !lost.is_empty() {
                let mut unread = Unread::of(&part.0, &lost);
                let behind = (&older_ids[..], &before);
                let found = (part, segment, &mut unread);
                if let Some(anew) = self.mended(repository, found, behind, mend)? {
                    stands = anew;
                }
                scrubbed.unread.push(unread);
            }
            older_ids.push(stands.0);
            older.push(stands.1);
        }
        Ok(scrubbed)
    }

    /// The live segments of the instance that do not give back every object,
    /// each with its repository, read as a scrub reads them. A backup holds the
    /// files of the segments to their digests, and a file whose index leads to
    /// no object, or to another, has the digest it was copied with: what is
    /// backed up and what was restored is read through before it is taken for
    /// a copy of the instance.
    pub fn unread_packs(&mut self, told: Reading<'_>) -> Result<Vec<(String, Unread)>> {
        Ok(self.unread_packs_besides(&Read::new(), told)?.0)
    }

    /// [`Self::unread_packs`] of the live segments that are not among those
    /// `done` already, each still read behind for those that follow it; and
    /// all that are live, by repository, as read by now.
    pub fn unread_packs_besides(
        &mut self,
        done: &Read,
        told: Reading<'_>,
    ) -> Result<(Vec<(String, Unread)>, Read)> {
        let mut now_read = Read::new();
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        let rows = tx.query("SELECT id FROM alt_repositories ORDER BY id", &[])?;
        tx.commit()?;
        let mut unread = Vec::new();
        for row in &rows {
            let repository: String = row.get(0);
            let (ids, set) = self.live_segments(&repository)?;
            let mut older: Vec<Arc<Segment>> = Vec::new();
            let (mut read, all) = (0, set.objects());
            let mut each = || {
                read += 1;
                told(&repository, read, all)
            };
            for (part, segment) in ids.iter().zip(set.segments()) {
                let known = (repository.clone(), part.0.clone());
                if !done.contains(&known) {
                    let before = SegmentSet::new(older.clone());
                    let lost = alt_pack_segment::unread_telling(segment, &before, &mut each);
                    let lost = lost.map_err(|_| Error::Integrity("pack segment index"))?;
                    if !lost.is_empty() {
                        unread.push((repository.clone(), Unread::of(&part.0, &lost)));
                    }
                }
                now_read.insert(known);
                older.push(Arc::clone(segment));
            }
        }
        Ok((unread, now_read))
    }

    /// A store of an instance at a schema this release upgrades from, or at
    /// the current one, for what is read of it before it is upgraded: every
    /// such schema has what [`Self::repositories`] and [`Self::refused_trees`]
    /// read, and nothing else is to be asked of the store.
    pub fn open_older(
        mut client: alt_pg::Client,
        bytes: crate::ByteStore,
        spec: crate::InstanceSpec,
        signing_key: alt_crypto::ed25519::SigningKey,
        limits: alt_remote_protocol::Limits,
    ) -> Result<Self> {
        spec.validate()?;
        limits.validate()?;
        let rows = client.query(
            "SELECT version,checksum FROM alt_schema ORDER BY version",
            &[],
        )?;
        let supported = crate::OLDEST_SCHEMA as usize..=crate::SCHEMA_VERSION as usize;
        if !supported.contains(&rows.len()) {
            return Err(Error::Precondition(crate::UNSUPPORTED_SOURCE_SCHEMA));
        }
        crate::schema::check_rows(&rows, rows.len())?;
        let client = crate::database::Connection::new(client);
        Ok(Store {
            client,
            bytes,
            spec,
            signing_key,
            limits,
            waited: None,
            restoring: None,
            held: None,
        })
    }

    /// The repositories of the instance, in the order of their names, for one
    /// who has the configuration and the database of the instance in hand.
    pub fn repositories(&mut self) -> Result<Vec<String>> {
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        let rows = tx.query("SELECT id FROM alt_repositories ORDER BY id", &[])?;
        tx.commit()?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    /// The trees of the live segments of `repository` that a push is now
    /// refused for: each with its segment, its id and why, in the words of
    /// `git fsck`. Nothing is changed, and nothing is read that schema 21 does
    /// not have: it is asked before an instance is upgraded.
    pub fn refused_trees(
        &mut self,
        repository: &str,
        told: Reading<'_>,
    ) -> Result<Vec<(String, String, &'static str)>> {
        let mut tx = self.client.transaction()?;
        instance_unlocked(&mut tx, &self.spec, &self.signing_key)?;
        let known = tx.query_opt("SELECT 1 FROM alt_repositories WHERE id=$1", &[&repository])?;
        if known.is_none() {
            return Err(Error::Precondition(NO_REPOSITORY));
        }
        let ids = Self::live_segment_ids(&mut tx, repository)?;
        tx.commit()?;
        let set = self.segments_as_they_are(repository, &ids)?;
        let (mut read, all, mut refused) = (0, set.objects(), Vec::new());
        let mut each = || {
            read += 1;
            told(repository, read, all)
        };
        for (at, (part, segment)) in ids.iter().zip(set.segments()).enumerate() {
            let before = SegmentSet::new(set.segments()[..at].to_vec());
            let found = alt_pack_segment::refused_trees(segment, &before, &mut each);
            for (tree, why) in found.map_err(|_| Error::Integrity("pack segment index"))? {
                refused.push((part.0.clone(), alt_pack_segment::hex(&tree), why));
            }
        }
        Ok(refused)
    }
}
