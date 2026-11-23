//! What the environment tells the server, read once before it listens.
//!
//! A number that does not read as one is an error the server ends with:
//! `ALT_SERVER_MAX_PUSH_BYTES=100M` taken for the default would be a
//! limit ten times the one that was meant.

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;

use super::http::Limits;

/// How many times the bytes a push may bring its entries and objects may
/// inflate to, all together, unless `ALT_SERVER_MAX_INFLATED_BYTES` says.
///
/// A history is packed as deltas, and what it inflates to is every
/// version of every file: some thirty times the bytes of the pack for the
/// history of a library, and more where large files change a little at a
/// time. Four times that is room for the histories there are, and still a
/// bound on what a pack made to be costly has the server hash.
const INFLATED_TIMES: u64 = 128;
/// How long what a peer still sends is taken on a connection that is
/// being ended.
const LINGER: Duration = Duration::from_secs(2);

pub(super) struct Told {
    /// `ALT_SERVER_MAX_PUSH_BYTES`: the bytes of the body of a request
    pub(super) push_bytes: u64,
    /// `ALT_SERVER_MAX_INFLATED_BYTES`: what the pack of a push inflates to
    pub(super) inflated_bytes: u64,
    /// `ALT_SERVER_MAX_INDEX_BYTES`: the memory the packs of pushes take
    /// between them while they are indexed; a push waits for it
    pub(super) index_bytes: u64,
    /// `ALT_SERVER_WORKERS`
    pub(super) workers: usize,
    /// `ALT_SERVER_MAX_CONNECTIONS`
    pub(super) connections: usize,
    /// `ALT_SERVER_MAX_OPEN_REPOS`: the repositories of a root kept open
    /// at once. One takes some forty files, with its store and four
    /// requests reading it, and a process has a thousand by default
    pub(super) repos: usize,
    /// `ALT_SERVER_IO_TIMEOUT_MS` and `ALT_SERVER_IO_MS_PER_MIB`
    pub(super) io: Limits,
    /// `ALT_SERVER_SHUTDOWN_DEADLINE_MS`
    pub(super) shutdown: Duration,
}

/// The number `name` is set to, above zero; `otherwise` where it is not
/// set.
fn number<T: std::str::FromStr + PartialOrd + Default>(
    lookup: &dyn Fn(&str) -> Option<OsString>,
    name: &str,
    otherwise: T,
) -> Result<T, String> {
    let Some(value) = lookup(name) else {
        return Ok(otherwise);
    };
    let read = value.to_str().and_then(|text| text.parse::<T>().ok());
    let read = read.filter(|number| *number > T::default());
    read.ok_or_else(|| format!("{name}={value:?}: a whole number above zero is expected"))
}

impl Told {
    pub(super) fn read(lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Told, String> {
        let push_bytes = number(lookup, "ALT_SERVER_MAX_PUSH_BYTES", 1u64 << 30)?;
        let inflated = push_bytes.saturating_mul(INFLATED_TIMES);
        // the largest object a push may bring three times over, and the
        // bases held beside it: room for one push at its worst
        let index = push_bytes.saturating_mul(3).saturating_add(1 << 30);
        let grace = number(lookup, "ALT_SERVER_IO_TIMEOUT_MS", 20_000u64)?;
        let grace = Duration::from_millis(grace);
        let per_mib = number(lookup, "ALT_SERVER_IO_MS_PER_MIB", 8_000u64)?;
        let shutdown = number(lookup, "ALT_SERVER_SHUTDOWN_DEADLINE_MS", 30_000u64)?;
        Ok(Told {
            push_bytes,
            inflated_bytes: number(lookup, "ALT_SERVER_MAX_INFLATED_BYTES", inflated)?,
            index_bytes: number(lookup, "ALT_SERVER_MAX_INDEX_BYTES", index)?,
            workers: number(lookup, "ALT_SERVER_WORKERS", 4usize)?,
            connections: number(lookup, "ALT_SERVER_MAX_CONNECTIONS", 256usize)?,
            repos: number(lookup, "ALT_SERVER_MAX_OPEN_REPOS", 16usize)?,
            io: Limits {
                grace,
                per_mib: Duration::from_millis(per_mib),
                linger: LINGER.min(grace),
            },
            shutdown: Duration::from_millis(shutdown),
        })
    }
}

/// What the environment of this process tells; the process ends where
/// it does not read.
pub(super) fn told() -> &'static Told {
    static TOLD: OnceLock<Told> = OnceLock::new();
    TOLD.get_or_init(|| {
        Told::read(&|name| std::env::var_os(name)).unwrap_or_else(|why| super::die(&why))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(set: &[(&str, &str)]) -> Result<Told, String> {
        Told::read(&|name| {
            let found = set.iter().find(|(known, _)| *known == name);
            found.map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    fn a_number_that_does_not_read_ends_the_start_and_is_not_taken_for_the_default() {
        let unset = with(&[]).unwrap();
        assert_eq!(unset.index_bytes, 4 << 30);
        assert_eq!(
            (unset.push_bytes, unset.inflated_bytes),
            (1 << 30, 128 << 30)
        );
        assert_eq!(
            (unset.workers, unset.connections, unset.repos),
            (4, 256, 16)
        );
        assert_eq!(unset.io.grace, Duration::from_secs(20));
        assert_eq!(unset.io.per_mib, Duration::from_secs(8));
        let names = [
            "ALT_SERVER_MAX_PUSH_BYTES",
            "ALT_SERVER_MAX_INFLATED_BYTES",
            "ALT_SERVER_MAX_INDEX_BYTES",
            "ALT_SERVER_WORKERS",
            "ALT_SERVER_MAX_CONNECTIONS",
            "ALT_SERVER_MAX_OPEN_REPOS",
            "ALT_SERVER_IO_TIMEOUT_MS",
            "ALT_SERVER_IO_MS_PER_MIB",
            "ALT_SERVER_SHUTDOWN_DEADLINE_MS",
        ];
        for name in names {
            for value in [
                "100M",
                "0",
                "-1",
                "",
                " 5",
                "1e3",
                "99999999999999999999999",
            ] {
                let why = with(&[(name, value)]).err().expect(value);
                assert!(why.starts_with(&format!("{name}={value:?}: ")), "{why}");
            }
            assert!(with(&[(name, "5")]).is_ok());
        }
        // the largest number there is counts on without wrapping
        let most = with(&[("ALT_SERVER_MAX_PUSH_BYTES", "18446744073709551615")]).unwrap();
        assert_eq!((most.push_bytes, most.inflated_bytes), (u64::MAX, u64::MAX));
    }
}
