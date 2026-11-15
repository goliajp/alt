//! Looking notes up many times over: a log asks for one per commit, and
//! reading the notes commit and its trees again for each would be most of
//! what the log does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use alt_git_codec::{HashAlgo, ObjectId, Tree};
use alt_repo::Repository;

use super::{Meta, NOTES_REF, Read, ReadResult, Res, commit_tree};

/// The notes of one notes commit. Its root tree is read once, each fan-out
/// tree the first time a commit under it is asked for; both stay as stored
/// and are searched in place.
pub struct Notes {
    algo: HashAlgo,
    root: Vec<u8>,
    dirs: RefCell<HashMap<ObjectId, Vec<u8>>>,
}

fn tree_data(read: &Read, id: ObjectId) -> Res<Vec<u8>> {
    Ok(read(id)?.ok_or("notes tree missing from store")?.1)
}

fn reader(repo: &Repository) -> impl Fn(ObjectId) -> ReadResult + '_ {
    move |id| Ok(repo.read_object(&id)?.map(|o| (o.kind, o.data)))
}

impl Notes {
    pub fn open(read: &Read, algo: HashAlgo, notes: ObjectId) -> Res<Self> {
        Ok(Self {
            algo,
            root: tree_data(read, commit_tree(read, notes)?)?,
            dirs: RefCell::default(),
        })
    }

    /// The notes of `repo`, when it has any.
    pub fn of(repo: &Repository) -> Res<Option<Self>> {
        match repo.resolve_ref(NOTES_REF)? {
            Some(tip) => Ok(Some(Self::open(&reader(repo), tip.algo(), tip)?)),
            None => Ok(None),
        }
    }

    /// The note for `target`, flat or fanned out.
    pub fn find(&self, read: &Read, target: ObjectId) -> Res<Option<Meta>> {
        let hex = target.to_string();
        let (fan, rest) = hex.as_bytes().split_at(2);
        let blob = match Tree::entry_named(&self.root, self.algo, hex.as_bytes())? {
            Some(blob) => Some(blob),
            None => match Tree::entry_named(&self.root, self.algo, fan)? {
                Some(dir) => {
                    let mut dirs = self.dirs.borrow_mut();
                    let tree = match dirs.entry(dir) {
                        Entry::Occupied(known) => known.into_mut(),
                        Entry::Vacant(new) => new.insert(tree_data(read, dir)?),
                    };
                    Tree::entry_named(tree, self.algo, rest)?
                }
                None => None,
            },
        };
        match blob {
            Some(b) => {
                let (_, data) = read(b)?.ok_or("note blob missing from store")?;
                Ok(Some(Meta::parse(&String::from_utf8_lossy(&data))))
            }
            None => Ok(None),
        }
    }

    /// The message the note of commit `oid` puts in place of the commit's
    /// own, if `notes` has one for it.
    pub fn message(notes: &Option<Self>, repo: &Repository, oid: ObjectId) -> Res<Option<String>> {
        let Some(notes) = notes else {
            return Ok(None);
        };
        Ok(notes.find(&reader(repo), oid)?.and_then(|m| m.message))
    }
}
