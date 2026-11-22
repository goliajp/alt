//! The time zone a caller is in: how far east of UTC its local time is at
//! each instant. It is read from what the caller's `TZ` names, or from the
//! system's `/etc/localtime`, without the C library, whose zone is one for
//! the whole process and cannot be a caller's while another thread asks
//! for another's.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

mod rule;
mod tzif;

use rule::Rule;

const ZONES: &str = "/usr/share/zoneinfo";
const SYSTEM: &str = "/etc/localtime";
const DAY: i64 = 86_400;

/// No file of the zone database is longer than this. The code the
/// database comes with takes at most 2000 changes, 256 kinds of local
/// time, 50 letters and 50 leap seconds from a table, which with both
/// tables of a file and its last line comes to less than 34 KiB.
const LONGEST: u64 = 64 << 10;

/// The days from the epoch to a day of the Gregorian calendar; `month`
/// is 1 to 12.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    // the year begins in March, so that the leap day ends it
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The year, the month and the day of the month that lie `days` from the
/// epoch.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let of_era = days.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// How far east of UTC local time is, in seconds, at every instant.
#[derive(Debug, PartialEq, Eq)]
pub struct Zone {
    /// the instants local time changes at, ascending, each with the
    /// offset from it on
    changes: Vec<(i64, i32)>,
    /// the offset before the first of them
    first: i32,
    /// what holds from the last of them on, where the zone says
    rule: Option<Rule>,
}

impl Zone {
    pub fn utc() -> Self {
        Zone {
            changes: Vec::new(),
            first: 0,
            rule: None,
        }
    }

    /// The zone a process with `tz` for its `TZ` is in, as the C library
    /// reads it: the system's zone where it is not set, UTC where it is
    /// empty, else the file of the zone database it names, with or
    /// without a colon before the name, else the POSIX rule it spells.
    /// What names neither is UTC, as it is to the C library.
    pub fn of(tz: Option<&str>) -> Self {
        Self::found(tz, Path::new(SYSTEM), Path::new(ZONES))
    }

    /// [`Self::of`], with the file of the system's zone at `system` and
    /// the files of the zone database under `zones`.
    fn found(tz: Option<&str>, system: &Path, zones: &Path) -> Self {
        let Some(tz) = tz else {
            return Self::read(system).unwrap_or_else(Self::utc);
        };
        let name = tz.strip_prefix(':').unwrap_or(tz);
        if name.is_empty() {
            return Self::utc();
        }
        let named = Self::read(&zones.join(name));
        let spelled = || Some(Self::ruled(rule::parse(name)?));
        named.or_else(spelled).unwrap_or_else(Self::utc)
    }

    /// The zone the file at `path` is of. What `TZ` names may be a pipe,
    /// a device or a file without end: only a plain file no longer than
    /// [`LONGEST`] is read, and nothing else is waited on.
    fn read(path: &Path) -> Option<Self> {
        let plain = |file: &std::fs::Metadata| file.is_file() && file.len() <= LONGEST;
        std::fs::metadata(path).ok().filter(plain)?;
        // a pipe put there since is not waited on for its writer
        let mut open = std::fs::File::options();
        let file = open.read(true).custom_flags(crate::sys::O_NONBLOCK);
        let file = file.open(path).ok()?;
        file.metadata().ok().filter(plain)?;
        let mut bytes = Vec::new();
        file.take(LONGEST + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 <= LONGEST).then(|| tzif::parse(&bytes))?
    }

    fn ruled(rule: Rule) -> Self {
        Zone {
            first: rule.standard(),
            changes: Vec::new(),
            rule: Some(rule),
        }
    }

    /// The seconds local time is east of UTC by at `instant`, which is
    /// in seconds since the epoch.
    pub fn offset_at(&self, instant: i64) -> i32 {
        let later = self.changes.partition_point(|(at, _)| *at <= instant);
        match (&self.rule, later.checked_sub(1)) {
            (Some(rule), _) if later == self.changes.len() => rule.offset_at(instant),
            (_, Some(last)) => self.changes[last].1,
            (_, None) => self.first,
        }
    }

    /// The offset of the local time that reads `wall`, the seconds its
    /// date and time of day would be from the epoch in UTC. A time the
    /// zone has twice is the earlier of the two, and one it skips is read
    /// by the offset before the skip.
    pub fn offset_of_local(&self, wall: i64) -> i32 {
        let before = self.offset_at(wall.saturating_sub(DAY));
        let after = self.offset_at(wall.saturating_add(DAY));
        let holds = |offset: i32| self.offset_at(wall.saturating_sub(offset.into())) == offset;
        if !holds(before) && holds(after) {
            return after;
        }
        before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_dates_turn_into_each_other() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2005, 4, 7), 12_880);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        for days in [
            -719_468, -1, 0, 58, 59, 60, 12_880, 19_782, 47_481, 2_932_896,
        ] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days, "{days}");
        }
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
    }

    fn zone(changes: &[(i64, i32)], first: i32) -> Zone {
        Zone {
            changes: changes.to_vec(),
            first,
            rule: None,
        }
    }

    #[test]
    fn the_offset_at_an_instant_is_that_of_the_last_change_before_it() {
        let zone = zone(&[(100, 3600), (200, 0)], -60);
        assert_eq!(zone.offset_at(99), -60);
        assert_eq!(zone.offset_at(100), 3600);
        assert_eq!(zone.offset_at(199), 3600);
        assert_eq!(zone.offset_at(200), 0);
        assert_eq!(Zone::utc().offset_at(5), 0);
    }

    #[test]
    fn a_local_time_had_twice_or_never_is_read_by_the_offset_before_the_change() {
        let hour = 3600;
        let change = 1_000_000;
        // the clock is put forward at `change`: the hour after it is skipped
        let forward = zone(&[(change, hour)], 0);
        assert_eq!(forward.offset_of_local(change - 1), 0);
        assert_eq!(forward.offset_of_local(change + 1800), 0);
        assert_eq!(forward.offset_of_local(change + 3600), hour);
        // and put back: the hour before it comes twice
        let back = zone(&[(change, 0)], hour);
        assert_eq!(back.offset_of_local(change - 1), hour);
        assert_eq!(back.offset_of_local(change + 1800), hour);
        assert_eq!(back.offset_of_local(change + 3600), 0);
    }

    /// Files of the zone database, as tzdata 2026f compiles them with
    /// every change listed to 2037 and the rule for the years after in
    /// the last line.
    const TOKYO: &[u8] = include_bytes!("zone/testdata/Asia_Tokyo");
    const NEW_YORK: &[u8] = include_bytes!("zone/testdata/America_New_York");
    const SAO_PAULO: &[u8] = include_bytes!("zone/testdata/America_Sao_Paulo");
    const DUBLIN: &[u8] = include_bytes!("zone/testdata/Europe_Dublin");
    const KATHMANDU: &[u8] = include_bytes!("zone/testdata/Asia_Kathmandu");
    const LORD_HOWE: &[u8] = include_bytes!("zone/testdata/Australia_Lord_Howe");
    const MONROVIA: &[u8] = include_bytes!("zone/testdata/Africa_Monrovia");

    /// A zone database with the one file `Asia/Tokyo`, and beside it the
    /// file of a system in New York.
    fn database() -> (crate::temp::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = crate::temp::tempdir().unwrap();
        let (system, zones) = (dir.path().join("localtime"), dir.path().join("zones"));
        std::fs::create_dir_all(zones.join("Asia")).unwrap();
        std::fs::write(zones.join("Asia/Tokyo"), TOKYO).unwrap();
        std::fs::write(&system, NEW_YORK).unwrap();
        (dir, system, zones)
    }

    #[test]
    fn tz_names_a_file_or_spells_a_rule_and_anything_else_is_utc() {
        let (dir, system, zones) = database();
        let found = |tz: Option<&str>| Zone::found(tz, &system, &zones);
        let tokyo = tzif::parse(TOKYO).unwrap();
        let by_path = zones.join("Asia/Tokyo").to_str().unwrap().to_owned();
        let after_a_colon = format!(":{by_path}");
        for named in ["Asia/Tokyo", ":Asia/Tokyo", &by_path, &after_a_colon] {
            assert_eq!(found(Some(named)), tokyo, "{named}");
        }
        for nothing in ["", ":", "Nowhere/Zone", ":Nowhere/Zone", "Asia", "Asia/"] {
            assert_eq!(found(Some(nothing)), Zone::utc(), "{nothing}");
        }
        assert_eq!(found(Some("JST-9")).offset_at(0), 9 * 3600);
        assert_eq!(found(Some(":<+0330>-3:30")).offset_at(0), 12_600);
        // a file stands before a rule of its name
        std::fs::write(zones.join("JST-9"), NEW_YORK).unwrap();
        assert_eq!(found(Some("JST-9")), tzif::parse(NEW_YORK).unwrap());
        drop(dir);
    }

    #[test]
    fn the_zone_of_the_system_is_read_where_tz_is_not_set() {
        let (dir, system, zones) = database();
        let new_york = tzif::parse(NEW_YORK).unwrap();
        assert_eq!(Zone::found(None, &system, &zones), new_york);
        // one that has no such file, or one that is no zone, is in UTC
        std::fs::write(&system, b"not a zone").unwrap();
        assert_eq!(Zone::found(None, &system, &zones), Zone::utc());
        std::fs::remove_file(&system).unwrap();
        assert_eq!(Zone::found(None, &system, &zones), Zone::utc());
        drop(dir);
    }

    /// What git wrote under glibc with `TZ` naming each zone, for a date
    /// without a zone in each season.
    #[test]
    fn the_files_of_the_zone_database_are_read() {
        let april = days_from_civil(2005, 4, 7) * DAY + 22 * 3600;
        let january = days_from_civil(2005, 1, 7) * DAY + 22 * 3600;
        for (name, file, in_april, in_january) in [
            ("Asia/Tokyo", TOKYO, 9 * 3600, 9 * 3600),
            ("America/New_York", NEW_YORK, -4 * 3600, -5 * 3600),
            ("America/Sao_Paulo", SAO_PAULO, -3 * 3600, -2 * 3600),
            ("Europe/Dublin", DUBLIN, 3600, 0),
            ("Asia/Kathmandu", KATHMANDU, 20_700, 20_700),
            ("Australia/Lord_Howe", LORD_HOWE, 37_800, 39_600),
        ] {
            let zone = tzif::parse(file).expect(name);
            assert_eq!(zone.offset_of_local(april), in_april, "{name}");
            assert_eq!(zone.offset_of_local(january), in_january, "{name}");
        }
        // past the last change a file lists, by the rule that ends it
        let new_york = tzif::parse(NEW_YORK).unwrap();
        let summer = days_from_civil(2090, 7, 1) * DAY + 12 * 3600;
        let winter = days_from_civil(2090, 1, 1) * DAY + 12 * 3600;
        assert_eq!(new_york.offset_of_local(summer), -4 * 3600);
        assert_eq!(new_york.offset_of_local(winter), -5 * 3600);
        // and before the zone kept whole minutes
        let monrovia = tzif::parse(MONROVIA).unwrap();
        let noon = days_from_civil(1971, 6, 1) * DAY + 12 * 3600;
        assert_eq!(monrovia.offset_of_local(noon), -2670);
    }

    /// A file of the first version with one kind of local time, an hour
    /// east, and `length` bytes in all: what follows the table is not
    /// looked at.
    fn padded(length: usize) -> Vec<u8> {
        let mut bytes = b"TZif".to_vec();
        bytes.extend([0; 16]);
        for count in [0u32, 0, 0, 0, 1, 1] {
            bytes.extend(count.to_be_bytes());
        }
        bytes.extend(3600i32.to_be_bytes());
        bytes.extend([0, 0, b'X']);
        assert!(bytes.len() <= length);
        bytes.resize(length, 0);
        bytes
    }

    #[test]
    fn what_tz_names_is_read_only_where_it_is_a_file_of_a_length_a_zone_has() {
        let dir = crate::temp::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
        let longest = LONGEST as usize;
        std::fs::write(at("short"), padded(64)).unwrap();
        std::fs::write(at("longest"), padded(longest)).unwrap();
        std::fs::write(at("longer"), padded(longest + 1)).unwrap();
        std::fs::write(at("huge"), padded(64)).unwrap();
        let huge = std::fs::File::options().write(true).open(at("huge"));
        huge.unwrap().set_len(1 << 40).unwrap();
        let pipe = std::ffi::CString::new(at("pipe")).unwrap();
        // SAFETY: the path is NUL-terminated and outlives the call
        assert_eq!(unsafe { crate::sys::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        std::os::unix::fs::symlink(at("pipe"), at("to-the-pipe")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", at("to-zero")).unwrap();

        for read in ["short", "longest"] {
            let zone = Zone::of(Some(&at(read)));
            assert_eq!(zone.offset_at(0), 3600, "{read}");
            assert_eq!(Zone::of(Some(&format!(":{}", at(read)))), zone, "{read}");
        }
        let began = std::time::Instant::now();
        let unread = [
            "longer",
            "huge",
            "pipe",
            "to-the-pipe",
            "to-zero",
            "",
            "none",
        ];
        let unread = unread.map(at).into_iter();
        let elsewhere = ["/dev/zero", "/dev/null", "/dev/urandom", "/"];
        for named in unread.chain(elsewhere.map(str::to_owned)) {
            assert_eq!(Zone::of(Some(&named)), Zone::utc(), "{named}");
            assert_eq!(Zone::of(Some(&format!(":{named}"))), Zone::utc(), "{named}");
        }
        // nothing was read to its end or waited on
        assert!(began.elapsed() < std::time::Duration::from_secs(20));
    }
}
