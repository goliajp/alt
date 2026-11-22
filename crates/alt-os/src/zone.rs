//! The time zone a caller is in: how far east of UTC its local time is at
//! each instant. It is read from what the caller's `TZ` names, or from the
//! system's `/etc/localtime`, without the C library, whose zone is one for
//! the whole process and cannot be a caller's while another thread asks
//! for another's.

use std::path::Path;

mod rule;
mod tzif;

use rule::Rule;

const ZONES: &str = "/usr/share/zoneinfo";
const SYSTEM: &str = "/etc/localtime";
const DAY: i64 = 86_400;

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
        let Some(tz) = tz else {
            return Self::read(Path::new(SYSTEM)).unwrap_or_else(Self::utc);
        };
        let name = tz.strip_prefix(':').unwrap_or(tz);
        if name.is_empty() {
            return Self::utc();
        }
        let named = Self::read(&Path::new(ZONES).join(name));
        let spelled = || Some(Self::ruled(rule::parse(name)?));
        named.or_else(spelled).unwrap_or_else(Self::utc)
    }

    fn read(path: &Path) -> Option<Self> {
        tzif::parse(&std::fs::read(path).ok()?)
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
        let before = self.offset_at(wall - DAY);
        let after = self.offset_at(wall + DAY);
        let holds = |offset: i32| self.offset_at(wall - i64::from(offset)) == offset;
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

    #[test]
    fn tz_names_a_file_or_spells_a_rule_and_anything_else_is_utc() {
        assert_eq!(Zone::of(Some("")), Zone::utc());
        assert_eq!(Zone::of(Some(":")), Zone::utc());
        assert_eq!(Zone::of(Some("Nowhere/Zone")), Zone::utc());
        let spelled = Zone::of(Some("JST-9"));
        assert_eq!(spelled.offset_at(0), 9 * 3600);
        assert_eq!(Zone::of(Some(":<+0330>-3:30")).offset_at(0), 12_600);
    }

    /// Each zone file is that of the system: what git wrote under glibc
    /// with `TZ` set so, for a date without a zone in each season.
    #[test]
    fn the_files_of_the_zone_database_are_read() {
        let april = days_from_civil(2005, 4, 7) * DAY + 22 * 3600;
        let january = days_from_civil(2005, 1, 7) * DAY + 22 * 3600;
        for (tz, in_april, in_january) in [
            ("Asia/Tokyo", 9 * 3600, 9 * 3600),
            (":Asia/Tokyo", 9 * 3600, 9 * 3600),
            ("/usr/share/zoneinfo/Asia/Tokyo", 9 * 3600, 9 * 3600),
            (":/usr/share/zoneinfo/Asia/Tokyo", 9 * 3600, 9 * 3600),
            ("America/New_York", -4 * 3600, -5 * 3600),
            ("EST5EDT", -4 * 3600, -5 * 3600),
            ("America/Sao_Paulo", -3 * 3600, -2 * 3600),
            ("Europe/Dublin", 3600, 0),
            ("Asia/Kathmandu", 20_700, 20_700),
            ("Australia/Lord_Howe", 37_800, 39_600),
            ("UTC", 0, 0),
        ] {
            let zone = Zone::of(Some(tz));
            assert_eq!(zone.offset_of_local(april), in_april, "{tz}");
            assert_eq!(zone.offset_of_local(january), in_january, "{tz}");
        }
        // past the last change a file lists, by the rule that ends it
        let new_york = Zone::of(Some("America/New_York"));
        let summer = days_from_civil(2090, 7, 1) * DAY + 12 * 3600;
        let winter = days_from_civil(2090, 1, 1) * DAY + 12 * 3600;
        assert_eq!(new_york.offset_of_local(summer), -4 * 3600);
        assert_eq!(new_york.offset_of_local(winter), -5 * 3600);
        // and before the zone kept whole minutes
        let monrovia = Zone::of(Some("Africa/Monrovia"));
        let noon = days_from_civil(1971, 6, 1) * DAY + 12 * 3600;
        assert_eq!(monrovia.offset_of_local(noon), -2670);
    }

    #[test]
    fn the_zone_of_the_system_is_read_where_tz_is_not_set() {
        let system = Zone::of(None);
        let named = std::fs::read_link(SYSTEM).ok().and_then(|target| {
            let target = target.to_str()?.to_owned();
            let name = target.split_once("zoneinfo/")?.1.to_owned();
            Some(Zone::of(Some(&name)))
        });
        if let Some(named) = named {
            assert_eq!(system, named);
        }
    }
}
