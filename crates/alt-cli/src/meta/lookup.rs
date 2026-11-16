//! Looking notes up many times over: a log asks for one per commit, and
//! reading the notes commit and its trees again for each would be most of
//! what the log does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;

use alt_git_codec::{HashAlgo, ObjectId, Tree};
use alt_repo::Repository;

use super::{Meta, NOTES_REF, Read, ReadResult, Res, commit_tree};

/// The notes of one notes commit. Its root tree is read once, each fan-out
/// tree the first time a commit under it is asked for. A log asks the root
/// for every commit, so its names are sorted for a binary search; a fan-out
/// tree is small and searched as stored.
pub struct Notes {
    algo: HashAlgo,
    root: Vec<u8>,
    /// where each root entry's name and object id lie in `root`, by name
    names: Vec<(Range<usize>, usize)>,
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
        let root = tree_data(read, commit_tree(read, notes)?)?;
        let mut names = Vec::new();
        let mut at = 0;
        for entry in Tree::raw_entries(&root, algo) {
            let (mode, name, oid) = entry?;
            let name_at = at + mode.as_bytes().len() + 1;
            let oid_at = name_at + name.len() + 1;
            names.push((name_at..name_at + name.len(), oid_at));
            at = oid_at + oid.len();
        }
        names.sort_unstable_by(|a, b| root[a.0.clone()].cmp(&root[b.0.clone()]));
        Ok(Self {
            algo,
            root,
            names,
            dirs: RefCell::default(),
        })
    }

    fn in_root(&self, name: &[u8]) -> Option<ObjectId> {
        let at = self
            .names
            .binary_search_by(|entry| self.root[entry.0.clone()].cmp(name))
            .ok()?;
        let oid = self.names[at].1;
        ObjectId::from_bytes(self.algo, &self.root[oid..oid + self.algo.raw_len()]).ok()
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
        let blob = match self.in_root(hex.as_bytes()) {
            Some(blob) => Some(blob),
            None => match self.in_root(fan) {
                Some(dir) => {
                    let mut dirs = self.dirs.borrow_mut();
                    let tree = match dirs.entry(dir) {
                        Entry::Occupied(known) => known.into_mut(),
                        Entry::Vacant(new) => new.insert(tree_data(read, dir)?),
                    };
                    Tree::find(tree, self.algo, rest)?.map(|(_, blob)| blob)
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

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use alt_git_codec::{EntryMode, HashAlgo, ObjectId, ObjectKind, Tree, TreeEntry};

    use super::super::{Meta, ReadResult, WriteResult, store};
    use super::Notes;

    const ALGO: HashAlgo = HashAlgo::Sha1;

    fn target(n: u32) -> ObjectId {
        ObjectId::hash_object(ALGO, ObjectKind::Blob, &n.to_le_bytes())
    }

    fn note(text: &str) -> Meta {
        Meta {
            fields: vec![("type".to_owned(), "feat".to_owned())],
            message: Some(format!("{text}\n")),
        }
    }

    #[test]
    fn a_log_reads_the_root_once_and_each_fan_out_tree_once() {
        let objects = RefCell::new(HashMap::new());
        let reads = Cell::new(0);
        let read = |id: ObjectId| -> ReadResult {
            reads.set(reads.get() + 1);
            Ok(objects.borrow().get(&id).cloned())
        };
        let mut write = |kind: ObjectKind, data: &[u8]| -> WriteResult {
            let id = ObjectId::hash_object(ALGO, kind, data);
            objects.borrow_mut().insert(id, (kind, data.to_vec()));
            Ok(id)
        };
        // fanned out, as alt writes them
        let mut tip = None;
        for n in 0..40 {
            let meta = note(&format!("note {n}"));
            tip = Some(
                store(
                    &read,
                    &mut write,
                    ALGO,
                    tip,
                    target(n),
                    &meta,
                    "a <a@b> 0 +0000",
                )
                .unwrap(),
            );
        }
        // and one flat entry in the root, as git writes them
        let flat = target(1000);
        let blob = write(ObjectKind::Blob, note("flat").render().as_bytes()).unwrap();
        let commit = objects.borrow()[&tip.unwrap()].1.clone();
        let root = alt_git_codec::Commit::parse(&commit)
            .unwrap()
            .tree()
            .unwrap();
        let mut tree = Tree::parse(&objects.borrow()[&root].1, ALGO).unwrap();
        tree.entries.push(TreeEntry {
            mode: EntryMode::from_bytes(b"100644").unwrap(),
            name: flat.to_string().as_str().into(),
            oid: blob,
        });
        let root = write(ObjectKind::Tree, &tree.serialize()).unwrap();
        let body = format!("tree {root}\nauthor a <a@b> 0 +0000\ncommitter a <a@b> 0 +0000\n\nx\n");
        let tip = write(ObjectKind::Commit, body.as_bytes()).unwrap();

        reads.set(0);
        let notes = Notes::open(&read, ALGO, tip).unwrap();
        assert_eq!(reads.get(), 2, "the notes commit and its root tree");
        for round in 0..2 {
            for n in 0..40 {
                let found = notes.find(&read, target(n)).unwrap().unwrap();
                assert_eq!(
                    found.message.as_deref(),
                    Some(format!("note {n}\n").as_str()),
                    "round {round}"
                );
            }
            assert_eq!(
                notes.find(&read, flat).unwrap().unwrap().message.as_deref(),
                Some("flat\n")
            );
            assert_eq!(notes.find(&read, target(5000)).unwrap(), None);
        }
        let dirs = tree.entries.iter().filter(|e| e.mode.is_tree()).count();
        // every find reads its note; a fan-out tree is read once however often it is asked
        let expected = 2 + 2 * 41 + dirs;
        assert_eq!(reads.get(), expected);
    }
}
