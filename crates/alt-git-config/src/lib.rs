//! Git config reading: full file syntax, git's value semantics
//! (bool/int conversions, last-one-wins), and `include`/`includeIf`
//! resolution. Pure-logic crate, business-agnostic.

mod include;
mod parse;
mod wildmatch;

use std::path::Path;

use bstr::{BStr, BString};

pub use include::IncludeContext;
pub use parse::parse_file;
pub use wildmatch::wildmatch;

/// One configuration line, fully qualified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Section name, lowercased (`core`, `branch`, …).
    pub section: String,
    /// Subsection, case-preserved (except the legacy dotted form).
    pub subsection: Option<BString>,
    /// Variable name, lowercased.
    pub key: String,
    /// `None` for the valueless boolean shorthand (`[x] flag`).
    pub value: Option<BString>,
}

impl Entry {
    /// The canonical dotted display name, as `git config --list` prints it.
    pub fn display_key(&self) -> String {
        match &self.subsection {
            Some(sub) => format!("{}.{}.{}", self.section, sub, self.key),
            None => format!("{}.{}", self.section, self.key),
        }
    }
}

/// A resolved configuration: entries in file order with includes spliced
/// in place, plus git's lookup and conversion semantics.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub entries: Vec<Entry>,
}

impl Config {
    /// Loads `path` and resolves `include` / `includeIf` directives.
    pub fn load(path: &Path, ctx: &IncludeContext) -> Result<Self, ConfigError> {
        Ok(Self {
            entries: include::load_with_includes(path, ctx)?,
        })
    }

    fn is_match(e: &Entry, section: &str, subsection: Option<&[u8]>, key: &str) -> bool {
        e.section == section
            && e.subsection.as_ref().map(|s| s.as_slice()) == subsection
            && e.key == key
    }

    /// Last value wins, like git. `Some(None)` = valueless true shorthand.
    pub fn get(
        &self,
        section: &str,
        subsection: Option<&[u8]>,
        key: &str,
    ) -> Option<Option<&BStr>> {
        self.entries
            .iter()
            .rev()
            .find(|e| Self::is_match(e, section, subsection, key))
            .map(|e| e.value.as_ref().map(|v| v.as_bstr()))
    }

    /// All values in order (multi-valued keys).
    pub fn get_all(
        &self,
        section: &str,
        subsection: Option<&[u8]>,
        key: &str,
    ) -> Vec<Option<&BStr>> {
        self.entries
            .iter()
            .filter(|e| Self::is_match(e, section, subsection, key))
            .map(|e| e.value.as_ref().map(|v| v.as_bstr()))
            .collect()
    }

    pub fn get_str(&self, section: &str, subsection: Option<&[u8]>, key: &str) -> Option<&BStr> {
        self.get(section, subsection, key).flatten()
    }

    /// git bool semantics: missing `=` → true; empty → false;
    /// yes/no/true/false/on/off (any case); otherwise int, nonzero = true.
    pub fn get_bool(
        &self,
        section: &str,
        subsection: Option<&[u8]>,
        key: &str,
    ) -> Option<Result<bool, ConfigError>> {
        self.get(section, subsection, key).map(|v| match v {
            None => Ok(true),
            Some(v) => parse_bool(v),
        })
    }

    /// git int semantics: optional sign, `k`/`m`/`g` scale by 1024.
    pub fn get_int(
        &self,
        section: &str,
        subsection: Option<&[u8]>,
        key: &str,
    ) -> Option<Result<i64, ConfigError>> {
        self.get(section, subsection, key).map(|v| match v {
            None => Err(ConfigError::Value("boolean shorthand is not an int")),
            Some(v) => parse_int(v),
        })
    }
}

fn parse_bool(v: &BStr) -> Result<bool, ConfigError> {
    if v.is_empty() {
        return Ok(false);
    }
    match v.to_ascii_lowercase().as_slice() {
        b"true" | b"yes" | b"on" => Ok(true),
        b"false" | b"no" | b"off" => Ok(false),
        _ => parse_int(v)
            .map(|i| i != 0)
            .map_err(|_| ConfigError::Value("not a valid boolean")),
    }
}

fn parse_int(v: &BStr) -> Result<i64, ConfigError> {
    const ERR: ConfigError = ConfigError::Value("not a valid integer");
    let s = v.trim();
    let (digits, scale): (&[u8], i64) = match s.last() {
        Some(b'k') | Some(b'K') => (&s[..s.len() - 1], 1 << 10),
        Some(b'm') | Some(b'M') => (&s[..s.len() - 1], 1 << 20),
        Some(b'g') | Some(b'G') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    let n: i64 = core::str::from_utf8(digits)
        .map_err(|_| ERR)?
        .parse()
        .map_err(|_| ERR)?;
    n.checked_mul(scale)
        .ok_or(ConfigError::Value("integer overflow"))
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io")]
    Io(#[from] std::io::Error),
    #[error("config syntax: {0}")]
    Syntax(&'static str),
    #[error("config value: {0}")]
    Value(&'static str),
    #[error("include depth exceeds {}", include::MAX_INCLUDE_DEPTH)]
    IncludeDepth,
}

// needed by parse_int / parse_bool
use bstr::ByteSlice;

#[cfg(test)]
mod tests {
    use super::*;

    fn config(s: &str) -> Config {
        Config {
            entries: parse_file(s.as_bytes()).unwrap(),
        }
    }

    #[test]
    fn lookup_last_wins_and_multi() {
        let c = config("[a]\nx = 1\nx = 2\n[b \"s\"]\nx = 3\n");
        assert_eq!(c.get_str("a", None, "x").unwrap().as_ref() as &[u8], b"2");
        assert_eq!(c.get_all("a", None, "x").len(), 2);
        assert_eq!(
            c.get_str("b", Some(b"s"), "x").unwrap().as_ref() as &[u8],
            b"3"
        );
        assert_eq!(c.get("a", Some(b"s"), "x"), None);
    }

    #[test]
    fn bool_semantics() {
        let c = config("[a]\nt\nf =\nyes = YES\noff = off\nnum = 3\nbad = maybe\n");
        let b = |k| c.get_bool("a", None, k).unwrap();
        assert!(b("t").unwrap());
        assert!(!b("f").unwrap());
        assert!(b("yes").unwrap());
        assert!(!b("off").unwrap());
        assert!(b("num").unwrap());
        assert!(b("bad").is_err());
    }

    #[test]
    fn int_semantics() {
        let c = config("[a]\nplain = -42\nkilo = 2k\nmega = 1M\nbad = 1x\n");
        let i = |k| c.get_int("a", None, k).unwrap();
        assert_eq!(i("plain").unwrap(), -42);
        assert_eq!(i("kilo").unwrap(), 2048);
        assert_eq!(i("mega").unwrap(), 1 << 20);
        assert!(i("bad").is_err());
    }
}
