//! The configuration that applies in a repository, read once for the
//! command that asks for it again and again.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alt_bytes::BString;
use alt_git_config::{Config, IncludeContext};

use super::{CALLER_ENV, Res, home, outer_listed, var};

/// The system's, the user's and on top the repository's configuration,
/// includes resolved, with the files they were read from or would be
/// once those exist.
pub(crate) struct Applying {
    pub(crate) config: Config,
    pub(crate) files: Vec<PathBuf>,
}

struct Held {
    /// which round of reading this is of ([`forget`])
    round: u64,
    dir: PathBuf,
    /// the caller's variables it was read under
    env: Vec<Option<OsString>>,
    /// the branch it was read on, where an `includeIf` goes by the branch
    branch: Option<Option<BString>>,
    read: Arc<Applying>,
}

static ROUND: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static HELD: RefCell<Option<Held>> = const { RefCell::new(None) };
}

/// Has every thread read the configuration again the next time it is
/// asked for: a new command begins, or a file of it was written.
pub fn forget() {
    ROUND.fetch_add(1, Ordering::Relaxed);
}

/// The configuration that applies in the repository that keeps its own
/// file as `config` in `dir`, as the caller of this thread reads it.
/// `branch` is asked for the branch checked out only where an `includeIf`
/// of the configuration goes by it.
pub(crate) fn applying(dir: &Path, branch: impl FnOnce() -> Option<BString>) -> Res<Arc<Applying>> {
    let round = ROUND.load(Ordering::Relaxed);
    let env: Vec<Option<OsString>> = CALLER_ENV.iter().map(|name| var(name)).collect();
    let held = HELD
        .take()
        .filter(|h| h.round == round && h.dir == dir && h.env == env);
    let held = match held {
        Some(held) => held,
        None => {
            let read = read(dir, None)?;
            let by_branch = read.config.entries.iter().any(|e| {
                let asks = |cond: &BString| cond.starts_with(b"onbranch:");
                e.section == "includeif" && e.subsection.as_ref().is_some_and(asks)
            });
            Held {
                round,
                dir: dir.to_owned(),
                env,
                branch: by_branch.then_some(None),
                read: Arc::new(read),
            }
        }
    };
    let held = match &held.branch {
        None => held,
        Some(on) => match branch() {
            now if now == *on => held,
            now => Held {
                read: Arc::new(read(dir, now.clone())?),
                branch: Some(now),
                ..held
            },
        },
    };
    let read = Arc::clone(&held.read);
    HELD.set(Some(held));
    Ok(read)
}

fn read(dir: &Path, branch: Option<BString>) -> Res<Applying> {
    let file = dir.join("config");
    let (mut entries, mut files) = outer_listed(Some(dir.to_owned()), branch.clone())?;
    if !file.is_file() {
        files.push(file);
        return Ok(Applying {
            config: Config { entries },
            files,
        });
    }
    let ctx = IncludeContext {
        git_dir: Some(dir.to_owned()),
        branch,
        home: home(),
    };
    let (own, read) = Config::load_listing(&file, &ctx)?;
    entries.extend(own.entries);
    files.extend(read);
    Ok(Applying {
        config: Config { entries },
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_settings::Caller;

    fn value(dir: &Path, branch: Option<&str>) -> Option<String> {
        let read = applying(dir, || branch.map(BString::from)).unwrap();
        let value = read.config.get_str("core", None, "fsync")?;
        Some(String::from_utf8(value.to_vec()).unwrap())
    }

    #[test]
    fn the_files_are_read_once_until_they_are_to_be_forgotten() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("config"), "[core]\n\tfsync = one\n").unwrap();
        assert_eq!(value(dir, None).as_deref(), Some("one"));
        std::fs::write(dir.join("config"), "[core]\n\tfsync = two\n").unwrap();
        assert_eq!(value(dir, Some("main")).as_deref(), Some("one"));
        forget();
        assert_eq!(value(dir, None).as_deref(), Some("two"));
        // another caller's files are not this one's
        let user = dir.join("user");
        std::fs::write(&user, "[core]\n\tfsync = theirs\n").unwrap();
        std::fs::remove_file(dir.join("config")).unwrap();
        let named = vec![("GIT_CONFIG_GLOBAL".to_owned(), user.display().to_string())];
        let caller = Caller::new(named);
        assert_eq!(value(dir, None).as_deref(), Some("theirs"));
        drop(caller);
        assert_eq!(value(dir, None), None);
    }

    #[test]
    fn what_is_included_by_branch_is_read_for_the_branch_asked_on() {
        let tmp = alt_os::temp::tempdir().unwrap();
        let dir = tmp.path();
        let own = "[core]\n\tfsync = any\n[includeIf \"onbranch:topic\"]\n\tpath = on-topic\n";
        std::fs::write(dir.join("config"), own).unwrap();
        std::fs::write(dir.join("on-topic"), "[core]\n\tfsync = topic\n").unwrap();
        forget();
        assert_eq!(value(dir, Some("main")).as_deref(), Some("any"));
        assert_eq!(value(dir, Some("topic")).as_deref(), Some("topic"));
        assert_eq!(value(dir, Some("topic")).as_deref(), Some("topic"));
        assert_eq!(value(dir, None).as_deref(), Some("any"));
    }
}
