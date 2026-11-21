//! Notes taken back on top of the notes as they are now: what a notes
//! commit changed is undone note by note, so that a note another writer
//! put on the shared ref since stays where it is.

use std::collections::BTreeMap;

use alt_git_codec::{EntryMode, HashAlgo, ObjectId, ObjectKind, Tree, TreeEntry};

use super::{Read, Res, Write, commit_tree, put_entry, tree_at};

/// A notes commit before a change and after it; none where there was no
/// notes ref.
pub type NotesChange = (Option<ObjectId>, Option<ObjectId>);

fn is_dir(entry: &TreeEntry) -> bool {
    entry.mode.as_bytes() == b"40000"
}

fn root_of(read: &Read, algo: HashAlgo, notes: Option<ObjectId>) -> Res<Tree> {
    match notes {
        Some(notes) => tree_at(read, algo, commit_tree(read, notes)?),
        None => Ok(Tree {
            entries: Vec::new(),
        }),
    }
}

/// The notes under one entry of a root tree, each by the name of its
/// commit: those of a fan-out directory, or the one of a flat entry.
fn notes_under(read: &Read, algo: HashAlgo, entry: &TreeEntry) -> Res<Vec<(String, ObjectId)>> {
    let fan = String::from_utf8_lossy(&entry.name).into_owned();
    if !is_dir(entry) {
        return Ok(vec![(fan, entry.oid)]);
    }
    let named = |e: &TreeEntry| (format!("{fan}{}", String::from_utf8_lossy(&e.name)), e.oid);
    Ok(tree_at(read, algo, entry.oid)?
        .entries
        .iter()
        .map(named)
        .collect())
}

/// The notes that are not the same in `old` and in `new`, each as it is
/// in the one and in the other.
fn differing(
    read: &Read,
    algo: HashAlgo,
    (old, new): NotesChange,
) -> Res<BTreeMap<String, NotesChange>> {
    let sides = [root_of(read, algo, old)?, root_of(read, algo, new)?];
    let mut found: BTreeMap<String, NotesChange> = BTreeMap::new();
    for (side, tree) in sides.iter().enumerate() {
        let other = &sides[1 - side];
        let same = |e: &&TreeEntry| {
            !other
                .entries
                .iter()
                .any(|o| o.name == e.name && o.oid == e.oid)
        };
        for entry in tree.entries.iter().filter(same) {
            for (name, blob) in notes_under(read, algo, entry)? {
                let note = found.entry(name).or_default();
                match side {
                    0 => note.0 = Some(blob),
                    _ => note.1 = Some(blob),
                }
            }
        }
    }
    found.retain(|_, (was, became)| was != became);
    Ok(found)
}

/// The note of the commit `name` in `root`.
fn note_at(read: &Read, algo: HashAlgo, root: &Tree, name: &str) -> Res<Option<ObjectId>> {
    let flat = |e: &&TreeEntry| !is_dir(e) && e.name == name.as_bytes();
    if let Some(entry) = root.entries.iter().find(flat) {
        return Ok(Some(entry.oid));
    }
    let Some((fan, rest)) = name.split_at_checked(2) else {
        return Ok(None);
    };
    let dir = |e: &&TreeEntry| is_dir(e) && e.name == fan.as_bytes();
    let Some(dir) = root.entries.iter().find(dir) else {
        return Ok(None);
    };
    let sub = tree_at(read, algo, dir.oid)?;
    let note = sub.entries.iter().find(|e| e.name == rest.as_bytes());
    Ok(note.map(|e| e.oid))
}

/// Puts the note of the commit `name` in `root` as `note`, under its
/// fan-out directory; none removes it.
fn set_note(
    read: &Read,
    write: &mut Write,
    algo: HashAlgo,
    root: &mut Tree,
    name: &str,
    note: Option<ObjectId>,
) -> Res<()> {
    root.entries
        .retain(|e| is_dir(e) || e.name != name.as_bytes());
    let Some((fan, rest)) = name.split_at_checked(2) else {
        return Ok(());
    };
    let dir = |e: &&TreeEntry| is_dir(e) && e.name == fan.as_bytes();
    let mut sub = match root.entries.iter().find(dir) {
        Some(dir) => tree_at(read, algo, dir.oid)?,
        None => Tree {
            entries: Vec::new(),
        },
    };
    sub.entries.retain(|e| e.name != rest.as_bytes());
    if let Some(note) = note {
        put_entry(&mut sub, rest, EntryMode::from_bytes(b"100644")?, note);
    }
    root.entries.retain(|e| e.name != fan.as_bytes());
    if !sub.entries.is_empty() {
        let sub = write(ObjectKind::Tree, &sub.serialize())?;
        put_entry(root, fan, EntryMode::from_bytes(b"40000")?, sub);
    }
    Ok(())
}

/// The notes commit that holds the tree `root` on top of `parent`.
pub(super) fn commit_on(
    write: &mut Write,
    root: ObjectId,
    parent: Option<ObjectId>,
    signature: &str,
) -> Res<ObjectId> {
    let mut body = format!("tree {root}\n");
    if let Some(p) = parent {
        body.push_str(&format!("parent {p}\n"));
    }
    body.push_str(&format!(
        "author {signature}\ncommitter {signature}\n\nNotes added by 'alt'\n"
    ));
    write(ObjectKind::Commit, body.as_bytes())
}

/// Undoes on top of the notes commit `tip` what each of `changes` did,
/// the last first: a note is put as it was before the change where it is
/// still as the change left it, and one written anew since stays, as
/// every note of another commit does. The notes commit that results;
/// none where there is nothing to undo.
pub fn take_back(
    read: &Read,
    write: &mut Write,
    algo: HashAlgo,
    tip: Option<ObjectId>,
    changes: &[NotesChange],
    signature: &str,
) -> Res<Option<ObjectId>> {
    let before = root_of(read, algo, tip)?;
    let mut root = Tree {
        entries: before.entries.clone(),
    };
    for change in changes.iter().rev() {
        for (name, (was, became)) in differing(read, algo, *change)? {
            if note_at(read, algo, &root, &name)? == became {
                set_note(read, write, algo, &mut root, &name, was)?;
            }
        }
    }
    if root.serialize() == before.serialize() {
        return Ok(None);
    }
    let root = write(ObjectKind::Tree, &root.serialize())?;
    commit_on(write, root, tip, signature).map(Some)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::super::{Meta, ReadResult, WriteResult, find, store};
    use super::*;

    const ALGO: HashAlgo = HashAlgo::Sha1;

    #[test]
    fn notes_are_taken_back_around_those_another_writer_put_between_and_after() {
        let objects: RefCell<HashMap<ObjectId, (ObjectKind, Vec<u8>)>> = RefCell::default();
        let read = |id: ObjectId| -> ReadResult { Ok(objects.borrow().get(&id).cloned()) };
        let write = |kind: ObjectKind, data: &[u8]| -> WriteResult {
            let id = ObjectId::hash_object(ALGO, kind, data);
            objects.borrow_mut().insert(id, (kind, data.to_vec()));
            Ok(id)
        };
        let commit = |byte: u8| ObjectId::Sha1([byte; 20]);
        let note = |text: &str| Meta::parse(&format!("summary: {text}\n"));
        let put = |on: Option<ObjectId>, target: u8, text: &str| {
            store(
                &read,
                &mut { write },
                ALGO,
                on,
                commit(target),
                &note(text),
                "a <a> 0 +0000",
            )
            .unwrap()
        };
        // two notes of one writer, with another's between them and after;
        // the commits 0x11 and 0x12 share a fan-out directory
        let found = put(None, 0x01, "there before");
        let first = put(Some(found), 0x11, "mine");
        let between = put(Some(first), 0x12, "theirs");
        let second = put(Some(between), 0x21, "mine too");
        let tip = put(Some(second), 0x31, "theirs later");
        let own = [(Some(found), Some(first)), (Some(between), Some(second))];
        let back = take_back(
            &read,
            &mut { write },
            ALGO,
            Some(tip),
            &own,
            "a <a> 0 +0000",
        );
        let back = back.unwrap().expect("two notes to take out");
        let has = |notes: ObjectId, target: u8| find(&read, ALGO, notes, commit(target)).unwrap();
        assert_eq!(has(back, 0x11), None);
        assert_eq!(has(back, 0x21), None);
        for (target, text) in [
            (0x01, "there before"),
            (0x12, "theirs"),
            (0x31, "theirs later"),
        ] {
            assert_eq!(has(back, target), Some(note(text)), "{target:#x}");
        }
        let parent = format!("parent {tip}\n");
        let stored = objects.borrow()[&back].1.clone();
        assert!(String::from_utf8(stored).unwrap().contains(&parent));

        // taking the taking back back puts the two notes there again
        let again = take_back(
            &read,
            &mut { write },
            ALGO,
            Some(back),
            &[(Some(tip), Some(back))],
            "",
        );
        let again = again.unwrap().expect("two notes to put back");
        assert_eq!(has(again, 0x11), Some(note("mine")));
        assert_eq!(has(again, 0x21), Some(note("mine too")));

        // a note written anew since is not the one to take out
        let rewritten = put(Some(tip), 0x11, "theirs now");
        let back = take_back(&read, &mut { write }, ALGO, Some(rewritten), &own, "");
        let back = back.unwrap().expect("one note to take out");
        assert_eq!(has(back, 0x11), Some(note("theirs now")));
        assert_eq!(has(back, 0x21), None);
        // and with nothing left to take out there is no notes commit to make
        assert_eq!(
            take_back(&read, &mut { write }, ALGO, Some(back), &own, "").unwrap(),
            None
        );
    }
}
