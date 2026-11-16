//! What the working tree holds that the index does not say: the scan
//! behind `status`, `diff` and `add`. Files whose stat is the one recorded
//! in the snapshot are not read; the rest are hashed, several at a time.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use alt_bytes::BString;
use alt_git_codec::{ObjectId, ObjectKind};

use crate::clock::Clock;
use crate::fsdir::{Kind, kind_of};
use crate::ignore::IgnoreStack;
use crate::scan::SubmoduleHead;
use crate::snapshot::{FileStat, Snapshot};
use crate::untracked::Untracked;
use crate::{WorkEntry, WorktreeError, walk};

/// Content read for comparison is held in memory at most this much at a time.
const BATCH_BYTES: u64 = 64 << 20;

/// A clean filter for a tracked file: given its path, the working-tree
/// bytes and the blob the index has for it, the bytes to store instead,
/// or `None` to store the file as it is.
pub type CleanTracked<'a> = &'a dyn Fn(&[u8], &[u8], ObjectId) -> Option<Vec<u8>>;

pub struct ScanOptions<'a> {
    /// which files the index does not have to look for
    pub untracked: Untracked,
    /// the paths to look at, each with everything beneath it; the whole
    /// tree when empty
    pub within: &'a [&'a [u8]],
    pub clean: Option<CleanTracked<'a>>,
    pub head: Option<SubmoduleHead<'a>>,
    /// the ignore files outside the working tree, a later one above an
    /// earlier and every `.gitignore` above them all
    pub excludes: &'a [&'a Path],
    /// when the scan began: a file changed at or after it is compared but
    /// not recorded as unchanged, because a later change within the same
    /// tick of the clock would leave its stat as it is
    pub clock: Clock,
}

/// The paths where the working tree differs from the index, each list in
/// path order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Delta {
    /// tracked paths whose file has other content or another mode, as the
    /// working tree has them
    pub changed: Vec<WorkEntry>,
    /// tracked paths with no file in place
    pub deleted: Vec<BString>,
    /// the files the index does not have; where the scan was asked to, a
    /// directory with nothing tracked in it stands for its files, as its
    /// path with a `/` at the end
    pub untracked: Vec<BString>,
    /// conflicted paths with a file in place
    pub unmerged: Vec<BString>,
    /// submodule paths whose directory holds a checked-out repository
    pub submodules: Vec<BString>,
}

impl FileStat {
    pub fn of(meta: &std::fs::Metadata) -> FileStat {
        FileStat {
            mtime: (meta.mtime(), meta.mtime_nsec() as u32),
            ctime: (meta.ctime(), meta.ctime_nsec() as u32),
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mode: meta.mode(),
        }
    }

    /// The git mode of a file or symlink with this stat.
    pub fn git_mode(&self) -> u32 {
        match kind_of(self.mode) {
            Kind::Link => 0o120000,
            _ if self.mode & 0o111 != 0 => 0o100755,
            _ => 0o100644,
        }
    }
}

/// The directories a scan that looked for untracked files came by.
#[derive(Default)]
pub(crate) struct Seen {
    /// those of the index that were listed, with every directory in them
    /// either among these as well or in one of the other two lists
    pub listed: Vec<Vec<u8>>,
    /// those not looked through to the end
    pub unwalked: Vec<Vec<u8>>,
    /// those an ignore rule hides, with nothing tracked in them
    pub passed: Vec<Vec<u8>>,
}

/// Compares the working tree at `root` with `snapshot`, recording in the
/// snapshot the stat of every file found to hold its entry's content.
pub fn scan(
    root: &Path,
    snapshot: &mut Snapshot,
    opts: &ScanOptions<'_>,
) -> Result<Delta, WorktreeError> {
    Ok(scan_seeing(root, snapshot, opts)?.0)
}

/// [`scan`], with the directories it came by.
pub(crate) fn scan_seeing(
    root: &Path,
    snapshot: &mut Snapshot,
    opts: &ScanOptions<'_>,
) -> Result<(Delta, Seen), WorktreeError> {
    let ignore = match opts.untracked {
        Untracked::No => IgnoreStack::new(),
        _ => IgnoreStack::of_files(opts.excludes)?,
    };
    let mut found = walk::walk(root, snapshot, opts.untracked, opts.within, &ignore)?;
    let view = &*snapshot;
    let listed = found
        .listed
        .iter()
        .map(|&d| view.dir_path(d as usize).to_vec());
    let links = found
        .gitlinks
        .iter()
        .map(|&i| view.path(i as usize).to_vec());
    let unwalked = std::mem::take(&mut found.unwalked);
    let seen = Seen {
        listed: listed.collect(),
        unwalked: unwalked.into_iter().chain(links).collect(),
        passed: std::mem::take(&mut found.passed),
    };
    let mut delta = Delta::default();
    found.suspects.sort_unstable_by_key(|s| s.0);
    compare(
        root,
        snapshot,
        opts,
        &found.suspects,
        &mut found.deleted,
        &mut delta,
    )?;
    for &i in &found.gitlinks {
        let i = i as usize;
        let path = snapshot.path(i);
        let dir = root.join(std::ffi::OsStr::from_bytes(path));
        let Some(oid) = opts.head.and_then(|head| head(&dir)) else {
            continue;
        };
        delta.submodules.push(BString::from(path));
        if oid != snapshot.entries[i].oid {
            delta.changed.push(WorkEntry {
                path: BString::from(path),
                oid,
                mode: 0o160000,
            });
        }
    }
    delta.submodules.sort();
    let paths = |entries: &mut Vec<u32>| {
        entries.sort_unstable();
        let mut paths: Vec<BString> = entries
            .iter()
            .map(|&i| BString::from(snapshot.path(i as usize)))
            .collect();
        paths.dedup();
        paths
    };
    delta.deleted = paths(&mut found.deleted);
    delta.unmerged = paths(&mut found.unmerged);
    found.untracked.sort_unstable();
    delta.untracked = found.untracked.into_iter().map(BString::from).collect();
    delta.changed.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((delta, seen))
}

/// A suspect's content as the working tree has it, with its blob id.
struct Read {
    content: Vec<u8>,
    oid: ObjectId,
}

/// Reads the files that do not show their recorded stat and sorts them
/// into changed, deleted and unchanged.
fn compare(
    root: &Path,
    snapshot: &mut Snapshot,
    opts: &ScanOptions<'_>,
    suspects: &[(u32, FileStat)],
    deleted: &mut Vec<u32>,
    delta: &mut Delta,
) -> Result<(), WorktreeError> {
    let algo = snapshot.algo;
    let mut rest = suspects;
    while !rest.is_empty() {
        let mut bytes = 0;
        let take = rest
            .iter()
            .take_while(|(_, st)| {
                bytes += st.size;
                bytes <= BATCH_BYTES
            })
            .count()
            .max(1);
        let (batch, later) = rest.split_at(take);
        rest = later;
        let view = &*snapshot;
        let read = |&(i, st): &(u32, FileStat)| -> std::io::Result<Option<Read>> {
            let path = root.join(std::ffi::OsStr::from_bytes(view.path(i as usize)));
            let content = match kind_of(st.mode) {
                Kind::Link => {
                    std::fs::read_link(&path).map(|t| t.into_os_string().into_encoded_bytes())
                }
                _ => std::fs::read(&path),
            };
            match content {
                Ok(content) => Ok(Some(Read {
                    oid: ObjectId::hash_object(algo, ObjectKind::Blob, &content),
                    content,
                })),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        };
        let grain = if batch.len() >= 64 { 4 } else { batch.len() };
        let reads = alt_threads::map(batch, grain, read);
        for (&(i, st), read) in batch.iter().zip(reads) {
            let Some(read) = read? else {
                deleted.push(i);
                continue;
            };
            let i = i as usize;
            let mode = st.git_mode();
            let staged = snapshot.entries[i].oid;
            let cleaned = match mode {
                0o120000 => None,
                _ => opts
                    .clean
                    .and_then(|f| f(snapshot.path(i), &read.content, staged)),
            };
            let oid = match cleaned {
                Some(stored) => ObjectId::hash_object(algo, ObjectKind::Blob, &stored),
                None => read.oid,
            };
            let entry = &snapshot.entries[i];
            if oid == entry.oid && mode == entry.mode {
                snapshot.settle(i, st, &opts.clock);
            } else {
                delta.changed.push(WorkEntry {
                    path: BString::from(snapshot.path(i)),
                    oid,
                    mode,
                });
            }
        }
    }
    Ok(())
}
