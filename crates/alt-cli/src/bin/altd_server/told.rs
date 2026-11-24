//! What the environment tells the server, read once before it listens.
//!
//! A number that does not read as one is an error the server ends with:
//! `ALT_SERVER_MAX_PUSH_BYTES=100M` taken for the default would be a
//! limit ten times the one that was meant.

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;

use super::http::Limits;

mod work;

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

/// What else the environment may give, as `--help` tells it. Each but the
/// first is a whole number above zero.
pub(super) const HELP: &str = "  \
ALT_SERVER_TEMP_DIR                     a directory on a disk for what is written for a time (the temporary files of the host)
  ALT_SERVER_WORKERS                      requests handled at once (4)
  ALT_SERVER_MAX_CONNECTIONS              connections open at once (256)
  ALT_SERVER_MAX_CONNECTIONS_PER_ADDRESS  of those, from one address that is not this host (a quarter)
  ALT_SERVER_MAX_PUSH_BYTES               bytes of the body of a push (1 GiB)
  ALT_SERVER_MAX_INFLATED_BYTES           bytes the pack of a push inflates to (128 times a push)
  ALT_SERVER_MAX_INDEX_BYTES              memory the packs of pushes are indexed in, and as much on disk (3 pushes and 1 GiB)
  ALT_SERVER_MAX_INDEX_WAIT_MS            how long a push waits for that memory before a 503 (600000)
  ALT_SERVER_MAX_RECEIVING_BYTES          bytes of request bodies received and not yet handled (4 pushes); a quarter for one address
  ALT_SERVER_MAX_SENDING_BYTES            bytes of responses to fetches written and not yet sent (half of what is free for temporary files); a quarter for one address
  ALT_SERVER_MAX_OPEN_REPOS               repositories of a root kept open (16)
  ALT_SERVER_IO_TIMEOUT_MS                for the head of a request to come (20000)
  ALT_SERVER_IO_WINDOW_MS                 in each of these a body or a response has to move (30000)
  ALT_SERVER_IO_MIN_BYTES_PER_SEC         by this much on average (1024)
  ALT_SERVER_SHUTDOWN_DEADLINE_MS         for requests under way when the server is stopped (30000)";

pub(super) struct Told {
    /// `ALT_SERVER_MAX_PUSH_BYTES`: the bytes of the body of a request
    pub(super) push_bytes: u64,
    /// `ALT_SERVER_MAX_INFLATED_BYTES`: what the pack of a push inflates to
    pub(super) inflated_bytes: u64,
    /// `ALT_SERVER_MAX_INDEX_BYTES`: the memory the packs of pushes take
    /// between them while they are indexed; a push waits for it
    pub(super) index_bytes: u64,
    /// `ALT_SERVER_MAX_INDEX_WAIT_MS`: how long a push waits for that
    /// memory before it is told to come again. It has a worker while it
    /// waits, and its body is sent anew when it comes again. What it
    /// waits for is the packs of the other workers, three by default,
    /// each indexed in under a minute where it is as large as a push may
    /// be: ten minutes is several times as long as they take together
    pub(super) index_wait: Duration,
    /// The bases of deltas that memory has no room for, in files in the
    /// directory this process has under `ALT_SERVER_TEMP_DIR` for what it
    /// writes for a time: as much there as in memory. Nothing before the
    /// process has made the directory
    pub(super) bases: Option<alt_git_pack::BaseFiles>,
    /// `ALT_SERVER_MAX_SENDING_BYTES`: the bytes the responses to fetches
    /// take on the disk between them until they are sent. Not set, it is
    /// zero here and [`told`] makes it half of what is free
    pub(super) sending_bytes: u64,
    /// `ALT_SERVER_WORKERS`
    pub(super) workers: usize,
    /// `ALT_SERVER_MAX_CONNECTIONS`
    pub(super) connections: usize,
    /// `ALT_SERVER_MAX_OPEN_REPOS`: the repositories of a root kept open
    /// at once. One takes some forty files, with its store and four
    /// requests reading it, and a process has a thousand by default
    pub(super) repos: usize,
    /// `ALT_SERVER_IO_TIMEOUT_MS` for the head of a request;
    /// `ALT_SERVER_IO_WINDOW_MS` and `ALT_SERVER_IO_MIN_BYTES_PER_SEC`
    /// for a body and a response to go on;
    /// `ALT_SERVER_MAX_CONNECTIONS_PER_ADDRESS`, a quarter of the
    /// connections unless it is set; `ALT_SERVER_MAX_RECEIVING_BYTES`,
    /// four bodies of the largest size unless it is set
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
        let window = number(lookup, "ALT_SERVER_IO_WINDOW_MS", 30_000u64)?;
        let rate = number(lookup, "ALT_SERVER_IO_MIN_BYTES_PER_SEC", 1024u64)?;
        let connections = number(lookup, "ALT_SERVER_MAX_CONNECTIONS", 256usize)?;
        let each = (connections / 4).max(1);
        let spool = push_bytes.saturating_mul(4);
        let shutdown = number(lookup, "ALT_SERVER_SHUTDOWN_DEADLINE_MS", 30_000u64)?;
        let index_wait = number(lookup, "ALT_SERVER_MAX_INDEX_WAIT_MS", 600_000u64)?;
        Ok(Told {
            push_bytes,
            inflated_bytes: number(lookup, "ALT_SERVER_MAX_INFLATED_BYTES", inflated)?,
            index_bytes: number(lookup, "ALT_SERVER_MAX_INDEX_BYTES", index)?,
            index_wait: Duration::from_millis(index_wait),
            bases: None,
            sending_bytes: number(lookup, "ALT_SERVER_MAX_SENDING_BYTES", 0u64)?,
            workers: number(lookup, "ALT_SERVER_WORKERS", 4usize)?,
            connections,
            repos: number(lookup, "ALT_SERVER_MAX_OPEN_REPOS", 16usize)?,
            io: Limits {
                grace,
                window: Duration::from_millis(window),
                floor: (rate.saturating_mul(window) / 1000).max(1),
                linger: LINGER.min(grace),
                each: number(lookup, "ALT_SERVER_MAX_CONNECTIONS_PER_ADDRESS", each)?,
                spool: number(lookup, "ALT_SERVER_MAX_RECEIVING_BYTES", spool)?,
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
        let mut told =
            Told::read(&|name| std::env::var_os(name)).unwrap_or_else(|why| super::die(&why));
        let made = work::open(std::env::var_os("ALT_SERVER_TEMP_DIR"))
            .and_then(|work| alt_git_pack::BaseFiles::open(&work.join("bases"), told.index_bytes));
        let unmade = |why| super::die(&format!("the directory for temporary files: {why}"));
        told.bases = Some(made.unwrap_or_else(unmade));
        if told.sending_bytes == 0 {
            let free = super::sending::half_of_what_is_free();
            let unread = |why| super::die(&format!("what is free for temporary files: {why}"));
            told.sending_bytes = free.unwrap_or_else(unread);
        }
        told
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
        assert_eq!(unset.index_wait, Duration::from_secs(600));
        assert_eq!(
            (unset.push_bytes, unset.inflated_bytes),
            (1 << 30, 128 << 30)
        );
        assert_eq!(
            (unset.workers, unset.connections, unset.repos),
            (4, 256, 16)
        );
        assert_eq!(unset.io.grace, Duration::from_secs(20));
        assert_eq!(unset.io.window, Duration::from_secs(30));
        assert_eq!((unset.io.floor, unset.io.each), (30 << 10, 64));
        assert_eq!(unset.io.spool, 4 << 30);
        let names = [
            "ALT_SERVER_MAX_PUSH_BYTES",
            "ALT_SERVER_MAX_INFLATED_BYTES",
            "ALT_SERVER_MAX_INDEX_BYTES",
            "ALT_SERVER_MAX_INDEX_WAIT_MS",
            "ALT_SERVER_WORKERS",
            "ALT_SERVER_MAX_CONNECTIONS",
            "ALT_SERVER_MAX_OPEN_REPOS",
            "ALT_SERVER_IO_TIMEOUT_MS",
            "ALT_SERVER_IO_WINDOW_MS",
            "ALT_SERVER_IO_MIN_BYTES_PER_SEC",
            "ALT_SERVER_MAX_CONNECTIONS_PER_ADDRESS",
            "ALT_SERVER_MAX_RECEIVING_BYTES",
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
