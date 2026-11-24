//! Two trees made at random of files, symlinks and directories that take
//! each other's places and names, checked out one after the other.

mod extras;

use super::{Bench, Leaf, Tree, alt_with, file, listing, there_and_back};

pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as usize % n
    }

    pub(super) fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len())]
    }

    pub(super) fn path(&mut self) -> String {
        let steps = 1 + self.below(3);
        let names: Vec<&str> = (0..steps)
            .map(|_| self.pick(&["a", "b", "c", "d"]))
            .collect();
        names.join("/")
    }

    pub(super) fn leaf(&mut self) -> Leaf {
        match self.below(5) {
            0 => Leaf::Link(
                self.pick(&["../decoy", "../decoy/sub", "b", "a/b", "gone"])
                    .to_owned(),
            ),
            _ => Leaf::File {
                text: self.pick(&["one\n", "two\n", "three\n"]).to_owned(),
                exec: self.below(4) == 0,
            },
        }
    }

    pub(super) fn held(&mut self, tree: &Tree) -> Option<String> {
        let paths: Vec<&String> = tree.keys().collect();
        (!paths.is_empty()).then(|| paths[self.below(paths.len())].clone())
    }
}

/// Puts `leaf` at `path`: what was above it as a file or beneath it as
/// a directory gives way.
pub(super) fn put(tree: &mut Tree, path: &str, leaf: Leaf) {
    let beneath = |inner: &str, outer: &str| {
        inner.len() > outer.len()
            && inner.starts_with(outer)
            && inner.as_bytes()[outer.len()] == b'/'
    };
    tree.retain(|held, _| !beneath(held, path) && !beneath(path, held));
    tree.insert(path.to_owned(), leaf);
}

pub(super) fn changed(rng: &mut Rng, tree: &mut Tree) {
    let Some(path) = rng.held(tree) else {
        return put(tree, &rng.path(), rng.leaf());
    };
    let leaf = tree[&path].clone();
    match rng.below(8) {
        0 => drop(tree.remove(&path)),
        1 => put(tree, &rng.path(), rng.leaf()),
        // moved, to where anything may be
        2 => {
            tree.remove(&path);
            put(tree, &rng.path(), leaf);
        }
        // a file becomes a directory that holds it
        3 => put(tree, &format!("{path}/{}", rng.pick(&["a", "b"])), leaf),
        // a directory becomes one of its files
        4 => {
            if let Some((above, _)) = path.rsplit_once('/') {
                put(tree, above, leaf);
            }
        }
        5 => {
            let other = rng.held(tree).expect("the tree holds a path");
            let theirs = tree.insert(other, leaf).expect("a held path");
            tree.insert(path, theirs);
        }
        6 => {
            if let Leaf::File { text, exec } = leaf {
                tree.insert(path, Leaf::File { text, exec: !exec });
            }
        }
        // a symlink becomes a directory another file moves into, or a
        // file moves away and leaves a symlink out of the tree
        _ => match leaf {
            Leaf::Link(_) => {
                let other = rng.held(tree).expect("the tree holds a path");
                let moved = tree.remove(&other).expect("a held path");
                put(tree, &format!("{path}/{}", rng.pick(&["a", "x"])), moved);
            }
            leaf => {
                put(tree, &path, Leaf::Link("../decoy".to_owned()));
                put(tree, &rng.path(), leaf);
            }
        },
    }
}

fn trees(seed: u64) -> (Tree, Tree) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut one = Tree::new();
    for _ in 0..6 + rng.below(5) {
        put(&mut one, &rng.path(), rng.leaf());
    }
    let mut two = one.clone();
    for _ in 0..3 + rng.below(6) {
        changed(&mut rng, &mut two);
    }
    if two == one || two.is_empty() {
        put(&mut two, "fresh", file("fresh\n"));
    }
    (one, two)
}

fn alt_ok(bench: &Bench, args: &[&str]) -> String {
    let done = alt_with(&bench.work, args, None);
    let said = String::from_utf8_lossy(&done.stderr).into_owned();
    assert!(done.status.success(), "{args:?}: {said}");
    String::from_utf8(done.stdout).unwrap()
}

/// Kills the switch from `main` to `two`, and the one back, at each
/// point they pass, and finishes each from the log: the tree is that of
/// the branch the repository is on, and the switch can be made.
pub(super) fn killed_at_each_point(bench: &Bench, one: &Tree, two: &Tree) {
    for (from, to) in [(("main", one), ("two", two)), (("two", two), ("main", one))] {
        alt_ok(bench, &["switch", from.0]);
        let list = bench.points();
        let _ = std::fs::remove_file(&list);
        let listed = format!("list:{}", list.display());
        let whole = alt_with(&bench.work, &["switch", to.0], Some(&listed));
        assert!(whole.status.success());
        let points = std::fs::read_to_string(&list).unwrap().lines().count();
        assert!(points > 2, "{points} points passed");
        for at in 1..=points {
            alt_ok(bench, &["switch", from.0]);
            bench.holds(from.1, &format!("before the kill at {at}"));
            let killed = alt_with(&bench.work, &["switch", to.0], Some(&at.to_string()));
            assert_eq!(killed.status.code(), None, "point {at} of {points}");
            loop {
                let pending = alt_ok(bench, &["operation", "pending"]);
                let Some(last) = pending.lines().last() else {
                    break;
                };
                let id = last.split(' ').next().unwrap().to_owned();
                alt_ok(bench, &["operation", "recover", &id]);
            }
            let on = alt_ok(bench, &["status"]);
            let (branch, tree) = match on.contains(&format!("On branch {}", to.0)) {
                true => to,
                false => from,
            };
            let found = listing(&bench.work);
            assert_eq!(&found, tree, "recovered on {branch} from the kill at {at}");
            alt_ok(bench, &["switch", to.0]);
            bench.holds(to.1, &format!("after the kill at {at}"));
        }
    }
}

#[test]
fn trees_made_at_random_are_checked_out_as_git_checks_them_out() {
    for seed in 1..=120 {
        let (one, two) = trees(seed);
        let run = std::panic::catch_unwind(|| {
            let bench = there_and_back(&one, &two);
            if seed % 10 == 0 {
                killed_at_each_point(&bench, &one, &two);
            }
        });
        assert!(run.is_ok(), "seed {seed}: {one:#?}\nto {two:#?}");
    }
}
