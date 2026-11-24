//! The memory the server takes, against what its container gives it.
//!
//! What the server takes has numbers of its own: the memory the packs of pushes
//! are verified in, the passwords checked at once, and what a worker reads and
//! writes of the request it answers. Its container has one number for all of it,
//! and ends the process that passes it. A configuration whose numbers add up to
//! more than the container has is one that pushes end the server with, so it is
//! refused when the server starts; and a configuration from before the first of
//! those numbers was a key is given one that fits when it is migrated.

use crate::operator::{Config, Error, Result};

/// the memory one password takes while it is checked, and how many are checked at once
const PASSWORD_BYTES: u64 = 64 << 20;
const PASSWORDS: u64 = 2;
/// what a worker holds of a request: a piece of a body it reads and a page it writes
const WORKER_BYTES: u64 = 16 << 20;
/// the memory of the application container of the package where nothing says more
const PACKAGED: u64 = 2 << 30;
const MEGABYTE: u64 = 1 << 20;

/// What the control group of this process may take, where it has a limit: inside
/// a container that is the limit of the container.
pub(in crate::operator) fn of_the_container() -> Option<u64> {
    let told = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok()?;
    told.trim().parse().ok()
}

/// A size as `ALT_MEMORY` and Docker write one: bytes, or a number with `k`,
/// `m` or `g` behind it.
fn written(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    let text = text.strip_suffix('b').unwrap_or(&text);
    let (digits, unit) = match text.char_indices().last()? {
        (at, 'k') => (&text[..at], 1 << 10),
        (at, 'm') => (&text[..at], 1 << 20),
        (at, 'g') => (&text[..at], 1 << 30),
        _ => (text, 1),
    };
    let digits = digits
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some(digits)?;
    digits.parse::<u64>().ok()?.checked_mul(unit)
}

/// The memory a migrated configuration is written for: what the container this
/// runs in has, or what `ALT_MEMORY` says where this runs in none, or what the
/// package gives its application.
fn to_write_for() -> Result<u64> {
    if let Some(limit) = of_the_container() {
        return Ok(limit);
    }
    match std::env::var("ALT_MEMORY") {
        Ok(told) => written(&told).ok_or(Error::Config("ALT_MEMORY is no size")),
        Err(_) => Ok(PACKAGED),
    }
}

impl Config {
    /// The memory the packs of pushes are verified in between them: what is
    /// told, or 4 GiB and no less than one pack at its worst takes.
    pub fn indexing_bytes(&self) -> u64 {
        match self.http.indexing_megabytes {
            Some(megabytes) => megabytes.saturating_mul(1 << 20),
            None => crate::git::least_verifying_bytes(self.limits).max(4 << 30),
        }
    }

    /// This configuration as it is migrated: one from before the memory for
    /// the packs of pushes was a key takes more for them than the container of
    /// the package has, and is given what goes with its container.
    pub(in crate::operator) fn for_its_container(&self) -> Result<Self> {
        let mut next = self.clone();
        if next.http.indexing_megabytes.is_none() {
            let megabytes = next.megabytes_for_packs(to_write_for()?);
            next.http.indexing_megabytes = Some(megabytes);
        }
        Ok(next)
    }

    /// what the server takes beside the packs of pushes
    fn beside_the_packs(&self) -> u64 {
        PASSWORDS * PASSWORD_BYTES + self.http.workers as u64 * WORKER_BYTES
    }

    /// The megabytes the packs of pushes are verified in that go with `memory`
    /// bytes for the whole server: three quarters of it less what else has a
    /// number, the rest being for what has none; no less than one pack of the
    /// largest object needs, and no more than the server takes where no key says.
    fn megabytes_for_packs(&self, memory: u64) -> u64 {
        let least = crate::git::least_verifying_bytes(self.limits).div_ceil(MEGABYTE);
        let fitting = (memory / 4 * 3).saturating_sub(self.beside_the_packs()) / MEGABYTE;
        fitting.clamp(least, least.max(4096))
    }

    /// Refuses a configuration that takes more than `memory`, what the container
    /// of the process may take.
    pub(in crate::operator) fn within(&self, memory: Option<u64>) -> Result<()> {
        let packs = match self.http.indexing_megabytes {
            Some(megabytes) => megabytes.saturating_mul(MEGABYTE),
            None => crate::git::least_verifying_bytes(self.limits).max(4 << 30),
        };
        match memory {
            Some(memory) if packs.saturating_add(self.beside_the_packs()) > memory => {
                Err(Error::Config(
                    "http.indexing_megabytes with the passwords and workers is more than the memory of the container (memory.max): lower the key, or raise the limit (ALT_MEMORY)",
                ))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Config {
        let text = include_str!("../../../example-config.json");
        alt_json::from_str(text).unwrap()
    }

    #[test]
    fn a_size_is_read_as_the_package_and_docker_write_it() {
        for (text, bytes) in [
            ("2g", 2 << 30),
            ("6G", 6 << 30),
            ("2048m", 2 << 30),
            ("1536MB", 1536 << 20),
            ("512k", 512 << 10),
            ("2147483648", 2 << 30),
            ("100b", 100),
        ] {
            assert_eq!(written(text), Some(bytes), "{text}");
        }
        for text in ["", "g", "2t", "-2g", "2.5g", "2 g", "+2g"] {
            assert_eq!(written(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_memory_for_packs_goes_with_the_memory_of_the_container() {
        let config = example();
        let gigabytes = |n: u64| n << 30;
        // the container of the package: the least the limits allow, as `prepare` writes
        assert_eq!(config.megabytes_for_packs(gigabytes(2)), 1376);
        assert_eq!(config.megabytes_for_packs(gigabytes(1)), 1376);
        // three quarters of four gigabytes less two passwords and eight workers
        assert_eq!(config.megabytes_for_packs(gigabytes(4)), 3072 - 128 - 128);
        // and no more than the server takes where no key says
        assert_eq!(config.megabytes_for_packs(gigabytes(8)), 4096);

        // no key is 4096 MiB: more than the container of the package has
        assert!(config.http.indexing_megabytes.is_none());
        assert!(matches!(
            config.within(Some(gigabytes(2))),
            Err(Error::Config(_))
        ));
        config.within(Some(gigabytes(6))).unwrap();
        config.within(None).unwrap();
        // what a migration writes for a container is within it
        for memory in [2, 3, 4, 6, 8, 64] {
            let mut written = example();
            let megabytes = written.megabytes_for_packs(gigabytes(memory));
            written.http.indexing_megabytes = Some(megabytes);
            written.within(Some(gigabytes(memory))).unwrap();
            // and no less than the server starts with
            assert!(megabytes << 20 >= crate::git::least_verifying_bytes(written.limits));
        }
        // the least there is does not fit one gigabyte, and the server says so
        let mut small = example();
        small.http.indexing_megabytes = Some(1376);
        assert!(small.within(Some(gigabytes(1))).is_err());
        small.within(Some(gigabytes(2))).unwrap();
    }
}
