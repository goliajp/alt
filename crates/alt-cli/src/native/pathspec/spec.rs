//! One path argument of a command that takes pathspecs, as git reads it:
//! with `:(magic)` or its short form in front, wildcards, and a `/` at
//! the end for a directory.

use std::path::Path;

use alt_bytes::BString;

use super::wild::Mode;
use super::{absolute, plain};
use crate::native::Res;
use crate::native::settings::fatal;

pub struct Spec {
    /// as the command was given it
    pub given: String,
    /// the path from the root, empty for the whole tree; no `/` at the end
    pub path: String,
    /// given with `/`, `.` or `..` at the end: what it names is a directory
    pub dir: bool,
    /// takes away from what the other paths name
    pub exclude: bool,
    literal: bool,
    glob: bool,
    fold_case: bool,
    /// how much of `path` is the directory the command ran in, with the
    /// `/` after it: matched as it is whatever the magic
    fixed: usize,
}

#[derive(Default)]
struct Magic {
    top: bool,
    literal: bool,
    glob: bool,
    fold_case: bool,
    exclude: bool,
}

impl Magic {
    /// The magic `given` begins with, and what follows it.
    fn of(given: &str) -> Res<(Magic, &str)> {
        let mut magic = Magic::default();
        let Some(marked) = given.strip_prefix(':') else {
            return Ok((magic, given));
        };
        if let Some(long) = marked.strip_prefix('(') {
            let Some((words, rest)) = long.split_once(')') else {
                let why = format!("Missing ')' at the end of pathspec magic in '{given}'");
                return Err(fatal(why));
            };
            for word in words.split(',') {
                match word {
                    "" => {}
                    "top" => magic.top = true,
                    "literal" => magic.literal = true,
                    "glob" => magic.glob = true,
                    "icase" => magic.fold_case = true,
                    "exclude" => magic.exclude = true,
                    _ => {
                        let why = format!("Invalid pathspec magic '{word}' in '{given}'");
                        return Err(fatal(why));
                    }
                }
            }
            return Ok((magic, rest));
        }
        let mut rest = marked;
        while let Some(mark) = rest.chars().next().filter(|c| *c != ':') {
            match mark {
                '/' => magic.top = true,
                '!' | '^' => magic.exclude = true,
                // the other characters git keeps for magic to come
                _ if "\"#%&',-;<=>@_`~".contains(mark) => {
                    let why = format!("Unimplemented pathspec magic '{mark}' in '{given}'");
                    return Err(fatal(why));
                }
                _ => return Ok((magic, rest)),
            }
            rest = &rest[mark.len_utf8()..];
        }
        Ok((magic, rest.strip_prefix(':').unwrap_or(rest)))
    }
}

impl Spec {
    /// What `given` names for a command run in the directory `prefix` of
    /// the tree at `root`.
    pub fn parse(root: &Path, prefix: &str, given: &str) -> Res<Spec> {
        if given.is_empty() {
            let hint = "please use . instead if you meant to match all paths";
            return Err(fatal(format!(
                "empty string is not a valid pathspec. {hint}"
            )));
        }
        let (magic, rest) = Magic::of(given)?;
        if magic.literal && magic.glob {
            return Err(fatal(format!(
                "{given}: 'literal' and 'glob' are incompatible"
            )));
        }
        // from the top a path is taken as it is written
        let path = match (magic.top, rest.starts_with('/')) {
            (true, _) => Some(rest.trim_end_matches('/').to_owned()),
            (false, true) => absolute(root, rest)?,
            (false, false) => plain(prefix, rest),
        };
        let Some(path) = path else {
            let root = root.display();
            return Err(fatal(format!(
                "{given}: '{rest}' is outside repository at '{root}'"
            )));
        };
        let from_here = !magic.top && !rest.starts_with('/') && !prefix.is_empty();
        let fixed = match path.strip_prefix(prefix) {
            Some("") if from_here => prefix.len(),
            Some(more) if from_here && more.starts_with('/') => prefix.len() + 1,
            _ => 0,
        };
        // a path that ends in `/`, `.` or `..` is a directory to git
        let last = rest.rsplit('/').next();
        let ends_dir = matches!(last, Some("" | "." | ".."));
        Ok(Spec {
            given: given.to_owned(),
            dir: ends_dir && !path.is_empty(),
            exclude: magic.exclude,
            literal: magic.literal,
            glob: magic.glob,
            fold_case: magic.fold_case,
            fixed,
            path,
        })
    }

    fn same(&self, a: &[u8], b: &[u8]) -> bool {
        match self.fold_case {
            true => a.eq_ignore_ascii_case(b),
            false => a == b,
        }
    }

    /// How much of the path is free of wildcards.
    fn plain_len(&self) -> usize {
        let wild = |b: &u8| matches!(b, b'*' | b'?' | b'[' | b'\\');
        let free = &self.path.as_bytes()[self.fixed..];
        match free.iter().position(wild).filter(|_| !self.literal) {
            Some(at) => self.fixed + at,
            None => self.path.len(),
        }
    }

    /// Whether the file `name` is what this names or lies beneath it. A
    /// `name` with `/` at the end is a directory reported as one path.
    pub fn matches(&self, name: &[u8]) -> bool {
        let path = self.path.as_bytes();
        let (file, is_dir) = match name.strip_suffix(b"/") {
            Some(dir) => (dir, true),
            None => (name, false),
        };
        let Some(free) = file.strip_prefix(&path[..self.fixed]) else {
            return false;
        };
        let rest = &path[self.fixed..];
        // what is left of the file once the path is taken off its front
        let named = |more: &[u8]| match more.first() {
            None => is_dir || !self.dir,
            Some(next) => *next == b'/',
        };
        if rest.is_empty() {
            return path.is_empty() || named(free);
        }
        if free.len() >= rest.len()
            && self.same(&free[..rest.len()], rest)
            && named(&free[rest.len()..])
        {
            return true;
        }
        let plain = self.plain_len();
        if plain == path.len() || self.dir {
            return false;
        }
        // git takes what is written out off both before it matches
        let (written, wild) = rest.split_at(plain - self.fixed);
        let mode = Mode {
            pathname: self.glob,
            fold_case: self.fold_case,
        };
        let tried = |free: &[u8]| match free.split_at_checked(written.len()) {
            Some((start, text)) => self.same(start, written) && mode.matches(wild, text, true),
            None => false,
        };
        // a directory reported as one path has its `/` for the wildcards
        tried(free) || is_dir && tried(&[free, b"/"].concat())
    }

    /// The directory a scan has to look at to find everything this can
    /// name: the path as far as it is written out; empty for the tree.
    pub fn reach(&self) -> &str {
        let plain = self.plain_len();
        match (self.fold_case, plain == self.path.len()) {
            (true, _) => self.path[..self.fixed].trim_end_matches('/'),
            (false, true) => &self.path,
            (false, false) => match self.path[..plain].rfind('/') {
                Some(slash) => &self.path[..slash],
                None => "",
            },
        }
    }

    /// The path as far as it is written out, which is what git goes by
    /// where it tells of a path an ignore rule hides.
    pub fn literal(&self) -> &str {
        self.path[..self.plain_len()].trim_end_matches('/')
    }

    /// The directory git walks for what this names: the one the path as
    /// far as it is written out lies in, or is when it names a directory.
    pub fn walked(&self) -> &str {
        let plain = self.plain_len();
        match (self.fold_case, self.dir && plain == self.path.len()) {
            (true, _) => self.path[..self.fixed].trim_end_matches('/'),
            (false, true) => &self.path,
            (false, false) => match self.path[..plain].rfind('/') {
                Some(slash) => &self.path[..slash],
                None => "",
            },
        }
    }

    /// Whether this is a path written out, which git looks up when
    /// nothing matched it.
    pub fn written_out(&self) -> bool {
        !self.glob && !self.fold_case
    }
}

/// Keeps of `names`, which are in order and not tracked, those the `specs`
/// name the way git goes through them: once a path given is, as it is
/// written, all of a name, it is tried on no name after that one. A path
/// that takes away is tried on every name.
pub fn keep_named(specs: &[Spec], names: &mut Vec<BString>) {
    let mut spent = vec![false; specs.len()];
    let none_names = !specs.is_empty() && specs.iter().all(|spec| spec.exclude);
    names.retain(|name| {
        let mut hit = [none_names, false];
        for takes_away in [false, true] {
            let tried = specs.iter().zip(&mut spent);
            for (spec, spent) in tried.filter(|(spec, _)| spec.exclude == takes_away) {
                if !*spent && spec.matches(name) {
                    hit[takes_away as usize] = true;
                    *spent = !takes_away && !spec.dir && spec.same(name, spec.path.as_bytes());
                }
            }
            if !hit[0] {
                return false;
            }
        }
        !hit[1]
    });
}

/// Whether `name` is among what the `specs` name together: what one of
/// them names and none takes away. Where all of them take away, they take
/// away from the whole tree.
pub fn named(specs: &[Spec], name: &[u8]) -> bool {
    let mut any = !specs.is_empty() && specs.iter().all(|spec| spec.exclude);
    for spec in specs {
        match (spec.exclude, spec.matches(name)) {
            (true, true) => return false,
            (false, true) => any = true,
            _ => {}
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(prefix: &str, given: &str) -> Spec {
        Spec::parse(Path::new("/r"), prefix, given).unwrap()
    }

    #[test]
    fn magic_is_read_in_its_long_and_its_short_form() {
        let top = spec("sub", ":/top.txt");
        assert_eq!((top.path.as_str(), top.exclude), ("top.txt", false));
        let colon = spec("sub", ":colon");
        assert_eq!(colon.path, "sub/colon");
        assert_eq!(spec("sub", ":").path, "sub");
        let not = spec("sub", ":!f.txt");
        assert_eq!((not.path.as_str(), not.exclude), ("sub/f.txt", true));
        assert!(spec("sub", ":^f.txt").exclude);
        let both = spec("sub", ":/!top.txt");
        assert_eq!((both.path.as_str(), both.exclude), ("top.txt", true));
        assert_eq!(spec("sub", ":(top,,glob)*.txt").path, "*.txt");
        // from the top a path is not put in order
        assert_eq!(spec("sub", ":(top)../x").path, "../x");
        for bad in [
            ":(bogus)x",
            ":(glob",
            ":(glob,literal)x",
            ":_x",
            "../..",
            "",
        ] {
            assert!(Spec::parse(Path::new("/r"), "sub", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_path_names_itself_what_lies_beneath_it_and_what_its_wildcards_match() {
        let hit =
            |prefix: &str, given: &str, name: &str| spec(prefix, given).matches(name.as_bytes());
        assert!(hit("sub", ".", "sub/deep/g.txt"));
        assert!(!hit("sub", ".", "subway/g.txt"));
        assert!(hit("sub", "deep/", "sub/deep/g.txt"));
        assert!(!hit("sub", "f.txt/", "sub/f.txt"));
        assert!(hit("sub", "mod/", "sub/mod/"));
        assert!(hit("sub", "*.txt", "sub/deep/g.txt"));
        assert!(!hit("sub", "*.txt", "other/o.txt"));
        assert!(!hit("sub", ":(glob)*.txt", "sub/deep/g.txt"));
        assert!(hit("sub", ":(glob)**/*.txt", "sub/deep/g.txt"));
        assert!(hit("sub", ":(top,glob)*.txt", "top.txt"));
        assert!(!hit("sub", ":(literal)*.txt", "sub/f.txt"));
        assert!(hit("sub", ":(literal)*.txt", "sub/*.txt"));
        assert!(hit("sub", ":(icase)DEEP/G.TXT", "sub/deep/g.txt"));
        assert!(!hit("Sub", ":(icase)f.txt", "sub/f.txt"));
        assert!(hit("sub/deep", ":(icase)../F.TXT", "sub/f.txt"));
        assert!(!hit("sub", "\\*.txt", "sub/f.txt"));
        assert!(hit("", ":/", "any/thing"));
        // `**` right after what is written out stands for any directories
        assert!(hit("sub", ":(glob)de**", "sub/deep/g.txt"));
        assert!(hit("", ":(top,glob)s**/n3.txt", "sub/deep/n3.txt"));
        assert!(!hit("sub", ":(glob)de*", "sub/deep/g.txt"));
        assert!(hit("sub", "inner/*", "sub/inner/"));
        assert!(hit("sub", ":(glob)inner/**", "sub/inner/"));
        assert!(!hit("sub", "deep/.", "sub/deep"));
    }

    #[test]
    fn what_some_paths_take_away_is_not_named() {
        let specs =
            |given: &[&str]| -> Vec<Spec> { given.iter().map(|g| spec("sub", g)).collect() };
        assert!(named(&specs(&[".", ":!f.txt"]), b"sub/g.txt"));
        assert!(!named(&specs(&[".", ":!f.txt"]), b"sub/f.txt"));
        assert!(!named(&specs(&[".", ":^deep"]), b"sub/deep/g.txt"));
        // nothing but what is taken away: from the whole tree
        assert!(named(&specs(&[":!f.txt"]), b"top.txt"));
        assert!(!named(&specs(&[":!"]), b"sub/f.txt"));
        assert!(!named(&specs(&["f.txt"]), b"top.txt"));
    }

    #[test]
    fn a_path_that_is_all_of_a_name_is_tried_on_no_later_name() {
        let kept = |given: &[&str], names: &[&str]| {
            let specs: Vec<Spec> = given.iter().map(|g| spec("sub", g)).collect();
            let mut names: Vec<BString> = names.iter().map(|n| BString::from(*n)).collect();
            keep_named(&specs, &mut names);
            names.iter().map(|n| n.to_string()).collect::<Vec<_>>()
        };
        let names = [
            "sub/!a.txt",
            "sub/*.txt",
            "sub/deep/n.txt",
            "sub/n.txt",
            "sub/x.md",
        ];
        assert_eq!(kept(&["*.txt"], &names), ["sub/!a.txt", "sub/*.txt"]);
        assert_eq!(
            kept(&["*.txt", "n.txt"], &names),
            ["sub/!a.txt", "sub/*.txt", "sub/n.txt"]
        );
        assert_eq!(kept(&["\\*.txt"], &names), ["sub/*.txt"]);
        assert_eq!(
            kept(&[":(icase)*.TXT"], &names),
            ["sub/!a.txt", "sub/*.txt"]
        );
        // what takes away goes on taking away
        assert_eq!(kept(&[".", ":!*.txt"], &names), ["sub/x.md"]);
        assert_eq!(kept(&[":!*.txt"], &names), ["sub/x.md"]);
    }

    #[test]
    fn a_scan_looks_where_the_path_is_written_out() {
        assert_eq!(spec("sub", "deep/g.txt").reach(), "sub/deep/g.txt");
        assert_eq!(spec("sub", "deep/*.txt").reach(), "sub/deep");
        assert_eq!(spec("sub", "*.txt").reach(), "sub");
        assert_eq!(spec("", "*.txt").reach(), "");
        assert_eq!(spec("sub", ":(icase)deep/g.txt").reach(), "sub");
        assert_eq!(spec("sub", ":(literal)*.txt").reach(), "sub/*.txt");
    }
}
