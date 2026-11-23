//! Which words of a command line are revisions and which are paths, told
//! apart as git tells them.
//!
//! With `--` written, what stands before it is revisions and what stands
//! after it paths, whatever either looks like. Without it a word is a
//! revision while it names one, and must then not be the name of a file
//! as well; from the first word that names none, every word is a path,
//! and must be one that is there.

use crate::native::said::refused;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// The words of a command that takes revisions and paths.
#[derive(Default)]
pub struct Words<'a> {
    /// those before `--`, all of them where it is not written
    pub before: &'a [String],
    /// whether `--` is written
    pub dashed: bool,
    /// those after `--`
    pub after: &'a [String],
}

/// What tells a word that names a revision, and one that names a file
/// that is there.
pub struct Tells<R, F> {
    pub revision: R,
    pub file: F,
}

const SEPARATE: &str = "Use '--' to separate paths from revisions, like this:\n\
                        'git <command> [<revision>...] -- [<file>...]'\n";

/// git's refusal of a word that names a revision and a file.
pub(crate) fn both(word: &str) -> Box<dyn std::error::Error> {
    let said = format!("fatal: ambiguous argument '{word}': both revision and filename\n");
    refused(&(said + SEPARATE), 128)
}

/// git's refusal of a word that names neither a revision nor a file.
pub(crate) fn neither(word: &str) -> Box<dyn std::error::Error> {
    let said = format!(
        "fatal: ambiguous argument '{word}': unknown revision or path not in the working tree.\n"
    );
    refused(&(said + SEPARATE), 128)
}

/// git's refusal of a word among the paths that names no file.
fn no_such_path(word: &str) -> Box<dyn std::error::Error> {
    let said = format!(
        "fatal: {word}: no such path in the working tree.\n\
         Use 'git <command> -- <path>...' to specify paths that do not exist locally.\n"
    );
    refused(&said, 128)
}

/// Whether `word` is a pattern, which names paths though no file has
/// its name.
fn is_pattern(word: &str) -> bool {
    word.starts_with(':') || word.contains(['*', '?', '['])
}

impl<'a> Words<'a> {
    /// Any number of revisions, then paths: the words of `log` and `diff`.
    pub(crate) fn revisions_then_paths<R, F>(
        &self,
        tells: &Tells<R, F>,
    ) -> Res<(&'a [String], &'a [String])>
    where
        R: Fn(&str) -> Res<bool>,
        F: Fn(&str) -> Res<bool>,
    {
        if self.dashed {
            for word in self.before {
                if !(tells.revision)(word)? {
                    return Err(refused(&format!("fatal: bad revision '{word}'\n"), 128));
                }
            }
            return Ok((self.before, self.after));
        }
        let mut revisions = 0;
        for word in self.before {
            if !(tells.revision)(word)? {
                break;
            }
            if (tells.file)(word)? {
                return Err(both(word));
            }
            revisions += 1;
        }
        let (revisions, paths) = self.before.split_at(revisions);
        for (at, word) in paths.iter().enumerate() {
            if is_pattern(word) || (tells.file)(word)? {
                continue;
            }
            return Err(match at {
                0 => neither(word),
                _ => no_such_path(word),
            });
        }
        Ok((revisions, paths))
    }

    /// One revision at most, the first word, then paths: the words of
    /// `reset`. Of the paths only the first must be there.
    pub(crate) fn revision_then_paths<R, F>(
        &self,
        tells: &Tells<R, F>,
    ) -> Res<(Option<&'a str>, &'a [String])>
    where
        R: Fn(&str) -> Res<bool>,
        F: Fn(&str) -> Res<bool>,
    {
        if self.dashed {
            return match self.before {
                [] => Ok((None, self.after)),
                [revision] => Ok((Some(revision), self.after)),
                _ => {
                    let usage = "only one revision may come before `--`";
                    Err(crate::machine::Failure::usage(usage).into())
                }
            };
        }
        let Some((first, rest)) = self.before.split_first() else {
            return Ok((None, self.before));
        };
        if (tells.revision)(first)? {
            if (tells.file)(first)? {
                return Err(both(first));
            }
            return Ok((Some(first), rest));
        }
        if is_pattern(first) || (tells.file)(first)? {
            return Ok((None, self.before));
        }
        Err(neither(first))
    }
}
