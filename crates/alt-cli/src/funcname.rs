//! What a hunk is part of, the text behind the `@@` of its header: the
//! nearest line above it that the diff driver of the file takes for the
//! start of a function or section. A driver is named by the `diff`
//! attribute of the path; its pattern comes from `diff.<name>.xfuncname`
//! or `diff.<name>.funcname`, or is one of the built-in ones.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use alt_bytes::ByteSlice;
use alt_git_config::Config;
use alt_regex::{Options, Regex, Syntax};
use alt_worktree::attributes::{self, AutoCrlf, CoreEol, DiffAttribute, EolConfig, Layer};

use crate::git_settings::{outer_attributes, read_if_file, user_attributes};

mod drivers;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// A driver that comes with the program.
pub(crate) struct Builtin {
    pub(crate) name: &'static str,
    pub(crate) ignore_case: bool,
    /// One expression each; `!` in front marks lines that start nothing.
    pub(crate) lines: &'static [&'static str],
}

/// The built-in drivers.
pub fn builtin() -> impl Iterator<Item = (&'static str, bool, &'static [&'static str])> {
    drivers::BUILTIN
        .iter()
        .map(|b| (b.name, b.ignore_case, b.lines))
}

/// One driver's pattern: the expressions of its lines, tried in order.
pub struct Finder {
    lines: Vec<Line>,
}

struct Line {
    /// a line this expression matches starts nothing
    negated: bool,
    expression: Regex,
    /// the expression's storage, kept here so that no search waits for it
    cache: RefCell<alt_regex::Cache>,
}

impl Finder {
    pub fn new(pattern: &[u8], options: Options) -> Res<Self> {
        let mut lines = Vec::new();
        let count = pattern.split(|&b| b == b'\n').count();
        for (at, line) in pattern.split(|&b| b == b'\n').enumerate() {
            let (negated, expression) = match line.strip_prefix(b"!") {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            if negated && at + 1 == count {
                let shown = String::from_utf8_lossy(line);
                return Err(format!("Last expression must not be negated: {shown}").into());
            }
            let compiled = Regex::with_options(expression, options).map_err(|_| {
                let shown = String::from_utf8_lossy(expression);
                format!("Invalid regexp to look for hunk header: {shown}")
            })?;
            lines.push(Line {
                negated,
                expression: compiled,
                cache: RefCell::default(),
            });
        }
        Ok(Finder { lines })
    }

    /// What the hunk header shows of `line` when it starts a section:
    /// the first group of the first expression that matches, or the
    /// whole match of one without a group. A line that a `!` expression
    /// matches first starts nothing.
    pub fn section<'l>(&self, line: &'l [u8]) -> Option<&'l [u8]> {
        let line = match line.strip_suffix(b"\n") {
            Some(rest) => rest.strip_suffix(b"\r").unwrap_or(rest),
            None => line,
        };
        for Line {
            negated,
            expression,
            cache,
        } in &self.lines
        {
            let mut cache = cache.borrow_mut();
            // a search given up is no match, as one the C library gives
            // up for want of memory is to git
            let found = expression.try_captures_upto_with(&mut cache, line, 1);
            let Ok(Some(found)) = found else {
                continue;
            };
            if *negated {
                return None;
            }
            let span = found.get(1).or(found.get(0)).expect("the whole match");
            return Some(alt_diff::section_text(&line[span]));
        }
        None
    }
}

/// The drivers of one repository, and which path has which.
pub(crate) struct Sections {
    root: Option<PathBuf>,
    /// the user's attributes file and the repository's own
    outer: Vec<Layer>,
    /// the `.gitattributes` of each directory asked about so far
    files: RefCell<HashMap<Vec<u8>, Option<Rc<Layer>>>>,
    configured: HashMap<Vec<u8>, (Vec<u8>, Syntax)>,
    finders: RefCell<HashMap<Vec<u8>, Option<Rc<Finder>>>>,
    /// lines are matched character by character, as git does in a
    /// UTF-8 locale
    utf8: bool,
}

impl Sections {
    /// The drivers under `settings`, for the working tree at `root`;
    /// `info` is the repository's own attributes file.
    pub(crate) fn new(root: Option<&Path>, info: &Path, settings: &Config) -> Res<Self> {
        let mut configured = HashMap::new();
        for e in settings.entries.iter().filter(|e| e.section == "diff") {
            let (Some(name), Some(value)) = (&e.subsection, &e.value) else {
                continue;
            };
            let syntax = match e.key.as_str() {
                "xfuncname" => Syntax::Extended,
                "funcname" => Syntax::Basic,
                _ => continue,
            };
            // the later of the two keys stands, whichever it is
            configured.insert(name.to_vec(), (value.to_vec(), syntax));
        }
        Ok(Sections {
            root: root.map(Path::to_path_buf),
            outer: outer_attributes(user_attributes(settings, root)?.as_deref(), info)?,
            files: RefCell::default(),
            configured,
            finders: RefCell::default(),
            utf8: crate::git_settings::utf8_locale(),
        })
    }

    fn file(&self, dir: &[u8]) -> Res<Option<Rc<Layer>>> {
        if let Some(known) = self.files.borrow().get(dir) {
            return Ok(known.clone());
        }
        let file = match (&self.root, dir.to_path()) {
            (Some(root), Ok(dir)) => read_if_file(root.join(dir).join(".gitattributes"))?,
            _ => None,
        };
        let layer = file
            .map(|content| Rc::new(Layer::parse(&content, dir)))
            .filter(|layer| !layer.is_empty());
        self.files.borrow_mut().insert(dir.to_vec(), layer.clone());
        Ok(layer)
    }

    /// What the attributes say of comparing `path`.
    fn attribute(&self, path: &[u8]) -> Res<DiffAttribute> {
        let mut files = Vec::new();
        files.extend(self.file(b"")?);
        for (at, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
            files.extend(self.file(&path[..at])?);
        }
        let layers = self.outer.iter().chain(files.iter().map(Rc::as_ref));
        let unused = EolConfig {
            autocrlf: AutoCrlf::False,
            eol: CoreEol::Native,
        };
        Ok(attributes::lookup(layers, path, &unused).diff)
    }

    /// Whether the two sides of a file are compared as binary: by the
    /// `diff` attribute of each side's path where it is set or unset,
    /// else by the content. A side that is not a regular file has no
    /// path here.
    pub(crate) fn binary(&self, paths: [Option<&[u8]>; 2], old: &[u8], new: &[u8]) -> Res<bool> {
        for (path, content) in paths.into_iter().zip([old, new]) {
            let said = path.map(|path| self.said_binary(path)).transpose()?;
            if said
                .flatten()
                .unwrap_or_else(|| alt_diff::is_binary(content))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// What the `diff` attribute of `path` says of comparing it as
    /// binary, when it is set or unset.
    pub(crate) fn said_binary(&self, path: &[u8]) -> Res<Option<bool>> {
        Ok(match self.attribute(path)? {
            DiffAttribute::Unset => Some(true),
            DiffAttribute::Set => Some(false),
            _ => None,
        })
    }

    /// The pattern of the driver `path` has; `None` when it has none and
    /// git's plain rule applies.
    pub(crate) fn of(&self, path: &[u8]) -> Res<Option<Rc<Finder>>> {
        let name = match self.attribute(path)? {
            DiffAttribute::Named(name) => name,
            // set without a name, a path has a driver of its own that
            // no pattern is configured for
            DiffAttribute::Set => return Ok(None),
            _ => b"default".to_vec(),
        };
        if let Some(known) = self.finders.borrow().get(&name) {
            return Ok(known.clone());
        }
        let builtin = || drivers::BUILTIN.iter().find(|b| b.name.as_bytes() == name);
        let finder = match (self.configured.get(&name), builtin()) {
            (Some((pattern, syntax)), _) => {
                let options = Options {
                    syntax: *syntax,
                    utf8: self.utf8,
                    ..Options::default()
                };
                Some(Finder::new(pattern, options)?)
            }
            (None, Some(builtin)) => {
                let options = Options {
                    ignore_case: builtin.ignore_case,
                    utf8: self.utf8,
                    ..Options::default()
                };
                Some(Finder::new(builtin.lines.join("\n").as_bytes(), options)?)
            }
            (None, None) => None,
        };
        let finder = finder.map(Rc::new);
        self.finders.borrow_mut().insert(name, finder.clone());
        Ok(finder)
    }

    /// Writes the hunks between two contents of a file, as
    /// [`alt_diff::write_unified`] does, their headers naming sections
    /// by the driver of the old path, or else of the new one. A side
    /// that is not a regular file has no path here.
    pub(crate) fn write_hunks(
        &self,
        out: &mut Vec<u8>,
        paths: [Option<&[u8]>; 2],
        old: &[u8],
        new: &[u8],
    ) -> Res<bool> {
        let mut finder = None;
        for path in paths.into_iter().flatten() {
            if finder.is_none() {
                finder = self.of(path)?;
            }
        }
        Ok(match finder {
            Some(finder) => {
                alt_diff::write_unified_with(out, old, new, 3, &|line| finder.section(line))
            }
            None => alt_diff::write_unified(out, old, new, 3),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finder(name: &str) -> Finder {
        let builtin = drivers::BUILTIN.iter().find(|b| b.name == name).unwrap();
        let options = Options {
            ignore_case: builtin.ignore_case,
            ..Options::default()
        };
        Finder::new(builtin.lines.join("\n").as_bytes(), options).unwrap()
    }

    #[test]
    fn every_built_in_pattern_compiles() {
        assert_eq!(drivers::BUILTIN.len(), 27);
        for builtin in drivers::BUILTIN {
            finder(builtin.name);
        }
    }

    #[test]
    fn a_driver_shows_its_group_and_skips_what_it_excludes() {
        let cpp = finder("cpp");
        let shown = |line: &str| cpp.section(line.as_bytes()).map(|s| s.to_vec());
        assert_eq!(
            shown("static int run(void)\r\n"),
            Some(b"static int run(void)".to_vec())
        );
        assert_eq!(shown("  indented(void)\n"), None);
        // a label is a line the first, negated expression takes away
        assert_eq!(shown("out:\n"), None);
        assert_eq!(shown("public:\n"), None);
        let python = finder("python");
        let shown = python.section(b"    def run(self):   \n").unwrap();
        assert_eq!(shown, b"def run(self):");
        let ada = finder("ada");
        assert_eq!(
            ada.section(b"  PROCEDURE Main is\n").unwrap(),
            b"PROCEDURE Main is"
        );
    }

    #[test]
    fn a_pattern_without_a_group_shows_the_whole_match_cut_to_eighty_bytes() {
        let finder = Finder::new(b"!^skip\n^[a-z]+ .*", Options::default()).unwrap();
        assert_eq!(finder.section(b"skip this line\n"), None);
        let long = format!("name {}\n", "x".repeat(200));
        assert_eq!(finder.section(long.as_bytes()).unwrap().len(), 80);
        assert!(Finder::new(b"^(a", Options::default()).is_err());
    }
}
