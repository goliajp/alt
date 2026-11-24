//! How far a command that reads every object of the instance has come, told
//! to the operator while it runs.
//!
//! `scrub`, `backup` and `restore` read every object of the pack segments, which
//! takes minutes on a large instance and prints nothing until it is done. A line
//! goes to standard error every so often; what the command found is on standard
//! output as before.

use std::time::{Duration, Instant};

/// the time between two lines
const EVERY: Duration = Duration::from_secs(10);

/// the clock is looked at once in this many objects
const LOOKED: u64 = 256;

pub struct Reading {
    command: &'static str,
    last: Instant,
}

impl Reading {
    pub fn by(command: &'static str) -> Self {
        Reading {
            command,
            last: Instant::now(),
        }
    }

    /// the line that is due at `now`, where one is
    fn due(&mut self, now: Instant, repository: &str, read: u64, all: u64) -> Option<String> {
        if now.duration_since(self.last) < EVERY {
            return None;
        }
        self.last = now;
        let command = self.command;
        Some(format!(
            "{command}: {read} of {all} objects of {repository} read"
        ))
    }

    /// Called as each object of a repository was read.
    pub fn read(&mut self, repository: &str, read: u64, all: u64) {
        if !read.is_multiple_of(LOOKED) && read != all {
            return;
        }
        if let Some(line) = self.due(Instant::now(), repository, read, all) {
            eprintln!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_due_every_so_often_and_not_between() {
        let mut reading = Reading::by("scrub");
        let began = reading.last;
        let at = |seconds: u64| began + Duration::from_secs(seconds);
        assert_eq!(reading.due(at(0), "one", 1, 9), None);
        assert_eq!(reading.due(at(9), "one", 2, 9), None);
        let line = reading.due(at(10), "one", 3, 9);
        assert_eq!(line.as_deref(), Some("scrub: 3 of 9 objects of one read"));
        assert_eq!(reading.due(at(19), "one", 4, 9), None);
        let line = reading.due(at(25), "two", 1, 2);
        assert_eq!(line.as_deref(), Some("scrub: 1 of 2 objects of two read"));
        assert_eq!(reading.due(at(34), "two", 2, 2), None);
    }
}
