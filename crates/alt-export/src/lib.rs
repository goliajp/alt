//! `alt export`: rebuild a `.git` from a native `.alt` store.
//!
//! L1 semantic fidelity by construction: objects are the canonical bytes
//! the store holds (their git ids cannot change), refs and HEAD come from
//! the native ref state, and the preserved `git-import/config` returns
//! verbatim. Pack layout is git's internal
//! freedom — the export writes one plain pack (L2 byte fidelity is
//! explicitly not pursued).
//!
//! The target directory must not exist or be empty: export never merges
//! into an existing repository (refreshing one is out of scope), and it
//! fails loudly rather than guessing.

use std::fs;
use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_config::Config;
use alt_git_pack::PackWriter;
use alt_odb::{NativeOdb, OdbError};
use alt_refs::{RefError, RefStore, RefTarget};
use bstr::BString;

#[derive(Debug)]
pub enum ExportError {
    Io(std::io::Error),
    Odb(OdbError),
    Refs(RefError),
    GitRefs(alt_git_refs::RefError),
    Pack(alt_git_pack::PackError),
    Config(alt_git_config::ConfigError),
    Format(&'static str),
    TargetNotEmpty(std::path::PathBuf),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => f.write_str("io"),
            Self::Odb(e) => std::fmt::Display::fmt(e, f),
            Self::Refs(e) => std::fmt::Display::fmt(e, f),
            Self::GitRefs(e) => std::fmt::Display::fmt(e, f),
            Self::Pack(e) => std::fmt::Display::fmt(e, f),
            Self::Config(e) => std::fmt::Display::fmt(e, f),
            Self::Format(v) => write!(f, "export format: {v}"),
            Self::TargetNotEmpty(v) => {
                write!(f, "export target {} exists and is not empty", v.display())
            }
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Odb(e) => e.source(),
            Self::Refs(e) => e.source(),
            Self::GitRefs(e) => e.source(),
            Self::Pack(e) => e.source(),
            Self::Config(e) => e.source(),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<OdbError> for ExportError {
    fn from(e: OdbError) -> Self {
        Self::Odb(e)
    }
}

impl From<RefError> for ExportError {
    fn from(e: RefError) -> Self {
        Self::Refs(e)
    }
}

impl From<alt_git_refs::RefError> for ExportError {
    fn from(e: alt_git_refs::RefError) -> Self {
        Self::GitRefs(e)
    }
}

impl From<alt_git_pack::PackError> for ExportError {
    fn from(e: alt_git_pack::PackError) -> Self {
        Self::Pack(e)
    }
}

impl From<alt_git_config::ConfigError> for ExportError {
    fn from(e: alt_git_config::ConfigError) -> Self {
        Self::Config(e)
    }
}

/// What one export produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    pub objects: u32,
    pub refs: usize,
    pub head: bool,
}

/// Exports the `.alt` store at `alt_dir` into `<target>/.git`. The target
/// directory may not exist yet or must be empty.
pub fn export_git(alt_dir: &Path, target: &Path) -> Result<ExportReport, ExportError> {
    match fs::read_dir(target) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(ExportError::TargetNotEmpty(target.to_owned()));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(target)?,
        Err(e) => return Err(e.into()),
    }
    let git_dir = target.join(".git");
    fs::create_dir_all(git_dir.join("refs"))?;

    // --- config first: it declares the object format ---
    let preserved = alt_dir.join("git-import/config");
    let algo;
    if preserved.is_file() {
        let bytes = fs::read(&preserved)?;
        let plain = Config {
            entries: alt_git_config::parse_file(&bytes)?,
        };
        algo = match plain.get_str("extensions", None, "objectformat") {
            None => HashAlgo::Sha1,
            Some(v) if v.as_ref() as &[u8] == b"sha256" => HashAlgo::Sha256,
            Some(_) => return Err(ExportError::Format("unknown extensions.objectFormat")),
        };
        // the export writes files-backend refs by decision, so a
        // preserved extensions.refstorage (e.g. reftable) must not
        // survive; everything else returns byte-identical
        fs::write(git_dir.join("config"), strip_refstorage(&bytes))?;
    } else {
        // native-born store without an import config: synthesize the
        // minimum a functional repository needs
        algo = HashAlgo::Sha1;
        fs::write(
            git_dir.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )?;
    }

    // --- objects: one plain pack with every mapped object. The bulk
    // decode-once read materializes each object once (a base shared by many
    // lineage deltas is decoded just once) without per-object re-hashing —
    // git fsck on the output is the integrity boundary. ---
    let odb = NativeOdb::open(alt_dir)?;
    let count = u32::try_from(odb.len()).map_err(|_| ExportError::Format("store too large"))?;
    let mut writer = PackWriter::create(&git_dir.join("objects/pack"), algo, count)?;
    let mut fail: Option<ExportError> = None;
    odb.for_each_object_unverified(|entry, data| {
        if fail.is_some() {
            return;
        }
        if entry.git.algo() != algo {
            fail = Some(ExportError::Format("store holds mixed hash algorithms"));
        } else if let Err(e) = writer.add(entry.git, entry.kind, data) {
            fail = Some(e.into());
        }
    })?;
    if let Some(e) = fail {
        return Err(e);
    }
    let written = writer.finish()?;

    // --- refs + HEAD from native state ---
    let refs = RefStore::open(alt_dir)?;
    let mut head = None;
    let mut loose: Vec<(BString, alt_git_refs::RefTarget)> = Vec::new();
    for (name, target) in refs.iter() {
        let converted = match target {
            RefTarget::Oid(oid) => alt_git_refs::RefTarget::Direct(*oid),
            RefTarget::Symbolic(sym) => {
                alt_git_refs::RefTarget::Symbolic(BString::from(sym.as_str()))
            }
        };
        if name == "HEAD" {
            head = Some(converted);
        } else {
            loose.push((BString::from(name), converted));
        }
    }
    let refs_written = loose.len();
    alt_git_refs::write_loose(&git_dir, head.as_ref(), &loose)?;

    Ok(ExportReport {
        objects: written.objects,
        refs: refs_written,
        head: head.is_some(),
    })
}

/// Drops `refstorage = …` lines inside `[extensions]`, byte-preserving
/// everything else.
fn strip_refstorage(config: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(config.len());
    let mut in_extensions = false;
    for line in config.split_inclusive(|&b| b == b'\n') {
        let trimmed = line.trim_ascii();
        if trimmed.starts_with(b"[") {
            in_extensions = trimmed.eq_ignore_ascii_case(b"[extensions]");
        }
        if in_extensions {
            let lower = trimmed.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix(b"refstorage")
                && rest.trim_ascii_start().starts_with(b"=")
            {
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    out
}
