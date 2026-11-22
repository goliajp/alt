//! What the paths given to a command name together, asked of many names.
//!
//! Most commands are given paths written out. Whether one of those names
//! a file is then a matter of the file's own path and the directories it
//! lies in, each looked up among the paths given, instead of every path
//! given held against every file.

use super::Spec;

/// The paths a command was given, ready to be asked about names.
pub struct Named<'a> {
    specs: &'a [Spec],
    /// where every path is written out and none takes away
    plain: Option<Plain<'a>>,
}

/// Paths written out, none of which takes away.
struct Plain<'a> {
    /// in order, each with whether it names a directory only
    paths: Vec<(&'a [u8], bool)>,
    /// one of them is the whole tree
    whole: bool,
    /// how long the shortest and the longest of the others are
    lens: (usize, usize),
}

impl<'a> Named<'a> {
    pub fn of(specs: &'a [Spec]) -> Self {
        let written = |spec: &Spec| !spec.exclude && spec.is_plain();
        let plain = specs.iter().all(written).then(|| {
            let paths = specs.iter().map(|spec| (spec.path.as_bytes(), spec.dir));
            let mut paths: Vec<_> = paths.collect();
            // of a path given twice, the one that names a file as well
            paths.sort_unstable();
            paths.dedup_by_key(|(path, _)| *path);
            let lens = paths.iter().map(|(path, _)| path.len());
            let lens = lens.filter(|len| *len > 0);
            Plain {
                whole: paths.first().is_some_and(|(path, _)| path.is_empty()),
                lens: (lens.clone().min().unwrap_or(1), lens.max().unwrap_or(0)),
                paths,
            }
        });
        Self { specs, plain }
    }

    /// Whether `name` is among what the paths name together: what one of
    /// them names and none takes away. Where all of them take away, they
    /// take away from the whole tree. A `name` with `/` at the end is a
    /// directory reported as one path.
    pub fn has(&self, name: &[u8]) -> bool {
        let Some(plain) = &self.plain else {
            return self.tried(name);
        };
        if plain.whole {
            return true;
        }
        let (paths, (shortest, longest)) = (&plain.paths, plain.lens);
        let (file, is_dir) = match name.strip_suffix(b"/") {
            Some(dir) => (dir, true),
            None => (name, false),
        };
        let given = |path: &[u8]| {
            let at = paths.binary_search_by(|(given, _)| (*given).cmp(path));
            at.ok().map(|at| paths[at].1)
        };
        // the name itself, or a directory it lies in: one as long as a
        // path given is
        let within = (shortest..=longest).contains(&file.len());
        within && given(file).is_some_and(|dir| is_dir || !dir)
            || (shortest..file.len().min(longest + 1))
                .any(|at| file[at] == b'/' && given(&file[..at]).is_some())
    }

    fn tried(&self, name: &[u8]) -> bool {
        let mut any = !self.specs.is_empty() && self.specs.iter().all(|spec| spec.exclude);
        for spec in self.specs {
            match (spec.exclude, spec.matches(name)) {
                (true, true) => return false,
                (false, true) => any = true,
                _ => {}
            }
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn paths_written_out_name_what_each_of_them_tried_in_turn_names() {
        let names = [
            "a",
            "a/",
            "a/b",
            "a/b/",
            "a/b/c.txt",
            "a-b",
            "a-b/c",
            "ab",
            "b",
            "sub",
            "sub/",
            "sub/f.txt",
            "sub/d/",
            "sub/d/f.txt",
            "sub/da",
            "subs/f.txt",
            "f.txt",
            "x[1].txt",
            "",
            "a//b",
        ];
        let given = [
            "a",
            "a/",
            "a/b",
            "a/b/c.txt",
            "b",
            "f.txt",
            ".",
            "d",
            "d/",
            "d/f.txt",
            "..",
            "../a",
            ":(top)a/b",
            ":(literal)x[1].txt",
            ":(glob)f.txt",
            ":/",
        ];
        for prefix in ["", "sub"] {
            // a path that leads out of the tree is no path to try
            let parse = |given: &&str| Spec::parse(Path::new("/r"), prefix, given).ok();
            for first in &given {
                for second in &given {
                    let specs: Vec<Spec> = [first, second].into_iter().filter_map(parse).collect();
                    let named = Named::of(&specs);
                    assert!(named.plain.is_some(), "{first} {second}");
                    for name in names {
                        let (name, case) = (name.as_bytes(), format!("{prefix}: {first} {second}"));
                        assert_eq!(named.has(name), named.tried(name), "{case}: {name:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_path_with_a_wildcard_or_one_that_takes_away_is_tried_as_it_is() {
        let parse = |given: &str| Spec::parse(Path::new("/r"), "", given).unwrap();
        for given in ["*.txt", ":!a", ":(icase)A", "a/?"] {
            let specs = [parse("a"), parse(given)];
            assert!(Named::of(&specs).plain.is_none(), "{given}");
        }
        let specs = [parse("a"), parse(":!a/b")];
        let named = Named::of(&specs);
        assert!(named.has(b"a/c") && !named.has(b"a/b") && !named.has(b"b"));
        assert!(!Named::of(&[]).has(b"a"));
    }
}
