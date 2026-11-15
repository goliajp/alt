//! Looking notes up many times over: a log asks for one per commit, and
//! reading the notes commit and its trees again for each would be most of
//! what the log does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use alt_git_codec::{HashAlgo, ObjectId, Tree};
use alt_repo::Repository;

use super::{Meta, NOTES_REF, Read, ReadResult, Res, commit_tree, tree_at};

/// The notes of one notes commit. Its root tree is read once, each fan-out
/// tree the first time a commit under it is asked for.
pub struct Notes {
    algo: HashAlgo,
    root: Tree,
    dirs: RefCell<HashMap<ObjectId, Tree>>,
}

fn reader(repo: &Repository) -> impl Fn(ObjectId) -> ReadResult + '_ {
    move |id| Ok(repo.read_object(&id)?.map(|o| (o.kind, o.data)))
}

impl Notes {
    pub fn open(read: &Read, algo: HashAlgo, notes: ObjectId) -> Res<Self> {
        Ok(Self {
            algo,
            root: tree_at(read, algo, commit_tree(read, notes)?)?,
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
        let named =
            |tree: &Tree, name: &[u8]| tree.entries.iter().find(|e| e.name == name).map(|e| e.oid);
        let blob = match named(&self.root, hex.as_bytes()) {
            Some(blob) => Some(blob),
            None => match named(&self.root, &hex.as_bytes()[..2]) {
                Some(dir) => {
                    let mut dirs = self.dirs.borrow_mut();
                    let tree = match dirs.entry(dir) {
                        Entry::Occupied(known) => known.into_mut(),
                        Entry::Vacant(new) => new.insert(tree_at(read, self.algo, dir)?),
                    };
                    named(tree, &hex.as_bytes()[2..])
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
