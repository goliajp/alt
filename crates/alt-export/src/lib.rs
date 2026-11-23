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

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use alt_bytes::BString;
use alt_git_codec::{HashAlgo, ObjectId};
use alt_git_config::Config;
use alt_git_pack::PackWriter;
use alt_odb::{NativeOdb, OdbError};
use alt_refs::{AlsoNamed, RefError, RefStore, RefTarget};

pub mod stash;

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
    /// A ref names an object the store does not hold.
    RefUnstored(String, ObjectId),
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
            Self::RefUnstored(name, oid) => write!(
                f,
                "ref {name} points at {oid}, which is not stored; nothing was exported"
            ),
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
    /// Refs left out when reading past damage: what they name is not
    /// stored.
    pub left_out: Vec<(String, ObjectId)>,
}

/// Exports the `.alt` store at `alt_dir` into `<target>/.git`. The target
/// directory may not exist yet or must be empty.
///
/// An export that fails takes back the `.git` written so far. A ref that
/// names an object the store does not hold fails it; a process that reads
/// past damage leaves such a ref out instead and reports it.
pub fn export_git(alt_dir: &Path, target: &Path) -> Result<ExportReport, ExportError> {
    export_git_naming(alt_dir, target, None)
}

/// [`export_git`] for a store whose log holds ops that are no ref
/// transactions and name objects, `also` telling which: an op that was
/// never synced and names what is not stored ends what is exported, as it
/// ends what every command that reads answers from.
pub fn export_git_naming(
    alt_dir: &Path,
    target: &Path,
    also: Option<&AlsoNamed>,
) -> Result<ExportReport, ExportError> {
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
    let report = write_git(alt_dir, &git_dir, also);
    if report.is_err() {
        fs::remove_dir_all(&git_dir)?;
    }
    report
}

/// Writes the config, which declares the object format: the preserved
/// one where there is one, the minimum a repository needs otherwise.
fn write_config(alt_dir: &Path, git_dir: &Path) -> Result<HashAlgo, ExportError> {
    let preserved = alt_dir.join("git-import/config");
    if !preserved.is_file() {
        // native-born store without an import config
        fs::write(
            git_dir.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )?;
        return Ok(HashAlgo::Sha1);
    }
    let bytes = fs::read(&preserved)?;
    let plain = Config {
        entries: alt_git_config::parse_file(&bytes)?,
    };
    let algo = match plain.get_str("extensions", None, "objectformat") {
        None => HashAlgo::Sha1,
        Some(v) if v.as_ref() as &[u8] == b"sha256" => HashAlgo::Sha256,
        Some(_) => return Err(ExportError::Format("unknown extensions.objectFormat")),
    };
    // the export writes files-backend refs by decision, so a preserved
    // extensions.refstorage (e.g. reftable) must not survive; everything
    // else returns byte-identical
    fs::write(git_dir.join("config"), strip_refstorage(&bytes))?;
    Ok(algo)
}

/// Writes one plain pack with every mapped object and takes each out of
/// `unstored`. The bulk decode-once read materializes each object once (a
/// base shared by many lineage deltas is decoded just once) without
/// per-object re-hashing: git fsck on the output is the integrity
/// boundary. Returns how many objects were written.
fn write_objects(
    odb: &NativeOdb,
    git_dir: &Path,
    algo: HashAlgo,
    unstored: &mut HashSet<ObjectId>,
) -> Result<u32, ExportError> {
    let count = u32::try_from(odb.len()?).map_err(|_| ExportError::Format("store too large"))?;
    let mut writer = PackWriter::create(&git_dir.join("objects/pack"), algo, count)?;
    let mut fail: Option<ExportError> = None;
    odb.for_each_object_unverified(|entry, data| {
        if fail.is_some() {
            return;
        }
        unstored.remove(&entry.git);
        if entry.git.algo() != algo {
            fail = Some(ExportError::Format("store holds mixed hash algorithms"));
        } else if let Err(e) = writer.add(entry.git, entry.kind, data) {
            fail = Some(e.into());
        }
    })?;
    if let Some(e) = fail {
        return Err(e);
    }
    Ok(writer.finish()?.objects)
}

fn write_git(
    alt_dir: &Path,
    git_dir: &Path,
    also: Option<&AlsoNamed>,
) -> Result<ExportReport, ExportError> {
    let algo = write_config(alt_dir, git_dir)?;
    let odb = NativeOdb::reading(alt_dir);
    // an object that does not read fails the export below
    let stored = odb.stored_past_cuts();
    let refs = RefStore::open_confirmed(alt_dir, also, |oid| stored(oid).unwrap_or(true))?;
    drop(stored);
    let mut unstored: HashSet<ObjectId> = refs
        .iter()
        .filter_map(|(_, target)| match target {
            RefTarget::Oid(oid) => Some(*oid),
            RefTarget::Symbolic(_) => None,
        })
        .collect();
    let objects = write_objects(&odb, git_dir, algo, &mut unstored)?;

    let mut head = None;
    let mut loose: Vec<(BString, alt_git_refs::RefTarget)> = Vec::new();
    let mut left_out = Vec::new();
    for (name, target) in refs.iter() {
        if let RefTarget::Oid(oid) = target
            && unstored.contains(oid)
        {
            if !alt_odb::reading_past_damage() {
                return Err(ExportError::RefUnstored(name.to_owned(), *oid));
            }
            left_out.push((name.to_owned(), *oid));
            continue;
        }
        let converted = match target {
            RefTarget::Oid(oid) => alt_git_refs::RefTarget::Direct(*oid),
            RefTarget::Symbolic(sym) => {
                alt_git_refs::RefTarget::Symbolic(BString::from(sym.as_str()))
            }
        };
        match name == "HEAD" {
            true => head = Some(converted),
            false => loose.push((BString::from(name), converted)),
        }
    }
    alt_git_refs::write_loose(git_dir, head.as_ref(), &loose)?;
    let entry = |(name, target): (&str, &RefTarget)| match target {
        RefTarget::Oid(oid) if !unstored.contains(oid) => Some((stash::entry_number(name)?, *oid)),
        _ => None,
    };
    stash::write_log(&odb, git_dir, refs.iter().filter_map(entry).collect(), algo)?;
    Ok(ExportReport {
        objects,
        refs: loose.len(),
        head: head.is_some(),
        left_out,
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
