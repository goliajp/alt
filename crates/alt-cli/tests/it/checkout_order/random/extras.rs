//! Trees made at random, checked out over a working tree that holds
//! what no tree does: files that are not tracked, files that are
//! ignored, directories with nothing in them and a second link to a
//! tracked file, in the directories the checkout moves and replaces.
//! Whatever git makes of each switch, a refusal or a tree, alt makes
//! the same.

use std::path::{Path, PathBuf};
use std::process::Output;

use super::super::{Leaf, Tree, alt, clear, file, listing, write_tree};
use super::{Rng, changed, put};
use crate::like_git::{git, ok};

type Run = fn(&Path, &[&str]) -> Output;

/// Moves all a directory of `tree` holds to another directory: several
/// files that go together.
fn moved_whole(rng: &mut Rng, tree: &mut Tree) {
    let Some(inside) = rng.held(tree).filter(|path| path.contains('/')) else {
        return;
    };
    let from = inside.split('/').next().unwrap().to_owned();
    let to = rng.pick(&["e", "f", "a/e", "g/h"]);
    let held: Vec<String> = tree.keys().cloned().collect();
    for path in held
        .iter()
        .filter(|path| path.starts_with(&format!("{from}/")))
    {
        let leaf = tree.remove(path).unwrap();
        put(tree, &format!("{to}{}", &path[from.len()..]), leaf);
    }
}

fn trees(seed: u64) -> (Tree, Tree, Rng) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut one = Tree::new();
    for _ in 0..6 + rng.below(5) {
        put(&mut one, &rng.path(), rng.leaf());
    }
    let mut two = one.clone();
    for _ in 0..2 + rng.below(4) {
        match rng.below(3) {
            0 => moved_whole(&mut rng, &mut two),
            _ => changed(&mut rng, &mut two),
        }
    }
    // files whose names end so are ignored, and on every other seed a
    // file is checked out with the line ends its attributes ask for
    let attributes = match seed % 2 {
        0 => "* text eol=crlf\n",
        _ => "",
    };
    for tree in [&mut one, &mut two] {
        tree.insert(".gitignore".to_owned(), file("*.ign\n"));
        tree.insert(".gitattributes".to_owned(), file(attributes));
    }
    (one, two, rng)
}

/// A repository of `run` in `place` with `one` on `main` and `two` on
/// the branch of that name, on `main`.
fn made(run: Run, place: &Path, one: &Tree, two: &Tree) -> PathBuf {
    let work = place.join("work");
    std::fs::create_dir_all(&work).unwrap();
    ok(run(&work, &["init", "."]));
    write_tree(&work, one);
    ok(run(&work, &["add", "-A"]));
    ok(run(&work, &["commit", "-m", "one"]));
    ok(run(&work, &["switch", "-c", "two"]));
    clear(&work);
    write_tree(&work, two);
    ok(run(&work, &["add", "-A"]));
    ok(run(&work, &["commit", "-m", "two"]));
    ok(run(&work, &["switch", "main"]));
    work
}

/// What is put into both working trees before a switch: paths with the
/// leaf of each, and second links as the path of the link and the file
/// it is one to.
struct Extras {
    put: Vec<(String, Leaf)>,
    linked: Vec<(String, String)>,
}

/// Things no tree holds, in directories of `here`, and at a path of
/// `there` that `here` has nothing at a second link to a file of `here`.
fn extras(rng: &mut Rng, here: &Tree, there: &Tree) -> Extras {
    let mut dirs: Vec<&str> = here
        .keys()
        .filter_map(|p| p.rsplit_once('/'))
        .map(|(d, _)| d)
        .collect();
    dirs.sort_unstable();
    dirs.dedup();
    let mut put = Vec::new();
    for dir in &dirs {
        if rng.below(2) == 0 {
            continue;
        }
        let leaf = match rng.below(4) {
            0 => (format!("{dir}/mine"), file("mine\n")),
            1 => (format!("{dir}/kept.ign"), file("ignored\n")),
            2 => (format!("{dir}/hollow"), Leaf::Empty),
            _ => (format!("{dir}/deep/er"), Leaf::Empty),
        };
        put.push(leaf);
    }
    let files = |tree: &Tree| -> Vec<String> {
        let plain = tree
            .iter()
            .filter(|(_, leaf)| matches!(leaf, Leaf::File { .. }));
        plain.map(|(path, _)| path.clone()).collect()
    };
    let (sources, mut linked) = (files(here), Vec::new());
    let free = |path: &&String| {
        let clear = |held: &String| {
            !held.starts_with(&format!("{path}/")) && !path.starts_with(&format!("{held}/"))
        };
        !here.contains_key(*path) && here.keys().all(clear)
    };
    let coming: Vec<&String> = there.keys().filter(free).collect();
    if !sources.is_empty() {
        let source = |rng: &mut Rng| sources[rng.below(sources.len())].clone();
        // one where the switch writes nothing, at a name no tree has
        linked.push((LINKED.to_owned(), source(rng)));
        // and on one round in three one where it writes: it is refused
        if !coming.is_empty() && rng.below(3) == 0 {
            let at = coming[rng.below(coming.len())].clone();
            linked.push((at, source(rng)));
        }
    }
    Extras { put, linked }
}

/// A name no tree made here has: the paths of a tree are of the letters
/// `a` to `d`.
const LINKED: &str = "second-link";

fn plant(work: &Path, extras: &Extras) {
    for (path, leaf) in &extras.put {
        let at = work.join(path);
        match leaf {
            Leaf::Empty => std::fs::create_dir_all(&at).unwrap(),
            Leaf::File { text, .. } => std::fs::write(&at, text).unwrap(),
            Leaf::Link(_) => unreachable!("no link is planted"),
        }
    }
    for (at, source) in &extras.linked {
        let at = work.join(at);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        // the one a switch before this left where it wrote nothing
        if at.symlink_metadata().is_ok() {
            std::fs::remove_file(&at).unwrap();
        }
        std::fs::hard_link(work.join(source), at).unwrap();
    }
}

/// Takes out of `work` what [`plant`] put there, after a switch that was
/// refused and so left it all: the next switch is not refused for it.
fn uproot(work: &Path, extras: &Extras) {
    for (path, leaf) in &extras.put {
        match leaf {
            Leaf::Empty => std::fs::remove_dir(work.join(path)).unwrap(),
            _ => std::fs::remove_file(work.join(path)).unwrap(),
        }
    }
    for (at, _) in &extras.linked {
        std::fs::remove_file(work.join(at)).unwrap();
    }
}

/// The switch to `branch` ends alike in both and leaves the same.
fn same_switch(works: &[PathBuf; 2], branch: &str, when: &str) -> bool {
    let theirs = git(&works[0], &["switch", branch]);
    let ours = alt(&works[1], &["switch", branch]);
    let said = String::from_utf8_lossy(&ours.stderr).into_owned();
    let git_said = String::from_utf8_lossy(&theirs.stderr).into_owned();
    assert_eq!(
        ours.status.success(),
        theirs.status.success(),
        "{when}: switch {branch}\nalt: {said}\ngit: {git_said}"
    );
    assert_eq!(
        listing(&works[1]),
        listing(&works[0]),
        "{when}: after switch {branch}"
    );
    assert_eq!(
        crate::like_git::index(&works[1]),
        ok(git(&works[0], &["ls-files", "-s"])),
        "{when}: the index after switch {branch}"
    );
    ours.status.success()
}

fn checked_out_alike(place: &Path, seeds: std::ops::RangeInclusive<u64>) {
    // the switches made, and those refused
    let mut ended = [0usize; 2];
    for seed in seeds {
        let (one, two, mut rng) = trees(seed);
        let dir = alt_os::temp::tempdir_in(place).unwrap();
        let run = std::panic::catch_unwind(move || {
            let made = |run: Run, name: &str| made(run, &dir.path().join(name), &one, &two);
            let works = [made(git, "git"), made(alt, "alt")];
            let (mut here, mut there) = (("main", &one), ("two", &two));
            let mut ended = [0usize; 2];
            for round in 0..3 {
                let extras = extras(&mut rng, here.1, there.1);
                works.iter().for_each(|work| plant(work, &extras));
                let when = format!("round {round} from {}", here.0);
                let switched = same_switch(&works, there.0, &when);
                ended[usize::from(!switched)] += 1;
                match switched {
                    true => std::mem::swap(&mut here, &mut there),
                    false => works.iter().for_each(|work| uproot(work, &extras)),
                }
            }
            ended
        });
        let Ok(of_seed) = run else {
            panic!("seed {seed}");
        };
        ended = [ended[0] + of_seed[0], ended[1] + of_seed[1]];
    }
    // both ends are met: a run of refusals alone would compare nothing
    assert!(ended[0] > 0 && ended[1] > 0, "{ended:?}");
}

#[test]
fn trees_made_at_random_are_checked_out_over_what_no_tree_holds_as_by_git() {
    checked_out_alike(&std::env::temp_dir(), 1..=150);
}

#[test]
#[ignore = "needs ALT_CASEFOLD_DIR: a directory on a file system that takes names differing in case for one"]
fn trees_made_at_random_are_checked_out_alike_where_names_fold_casefold() {
    checked_out_alike(&crate::casefold::place(), 1..=60);
}
