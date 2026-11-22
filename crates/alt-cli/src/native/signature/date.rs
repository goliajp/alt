//! The date `GIT_AUTHOR_DATE` or `GIT_COMMITTER_DATE` gives, in the three
//! forms git documents for them: its own (`<seconds> <zone>`, with or
//! without `@` before it), ISO 8601 and RFC 2822. A date that names no
//! zone is read as UTC, the zone every date made here is written in.

/// The seconds since the epoch and the zone as `±hhmm`.
pub(super) fn parse(text: &str) -> Option<(i64, String)> {
    let text = text.trim();
    raw(text).or_else(|| iso(text)).or_else(|| rfc2822(text))
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn eat(&mut self, byte: u8) -> bool {
        let found = self.0.first() == Some(&byte);
        if found {
            self.0 = &self.0[1..];
        }
        found
    }

    /// A number of `least` digits or more, of which `most` are taken.
    fn number(&mut self, least: usize, most: usize) -> Option<i64> {
        let digits = self.0.iter().take_while(|b| b.is_ascii_digit());
        let digits = digits.take(most).count();
        if digits < least {
            return None;
        }
        let (number, rest) = self.0.split_at(digits);
        self.0 = rest;
        std::str::from_utf8(number).ok()?.parse().ok()
    }

    fn spaces(&mut self) -> bool {
        let spaces = self.0.iter().take_while(|b| **b == b' ').count();
        self.0 = &self.0[spaces..];
        spaces > 0
    }

    /// `hh:mm`, with `:ss` and a fraction of a second, which is dropped,
    /// where they are given: the seconds of the day.
    fn time(&mut self) -> Option<i64> {
        let hour = self.number(2, 2)?;
        let minute = self.eat(b':').then(|| self.number(2, 2))??;
        let second = match self.eat(b':') {
            true => self.number(2, 2)?,
            false => 0,
        };
        if self.eat(b'.') {
            self.number(1, 18)?;
        }
        (hour < 24 && minute < 60 && second <= 60).then_some(hour * 3600 + minute * 60 + second)
    }

    /// What is left as a zone, `Z`, `±hhmm`, `±hh:mm` or `±hh`, or the one
    /// for `absent` where nothing is: the seconds it is east of UTC by.
    fn zone(mut self, absent: Option<i64>) -> Option<i64> {
        if self.0.is_empty() {
            return absent;
        }
        if self.0 == b"Z" {
            return Some(0);
        }
        let sign = match self.0.first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        self.0 = &self.0[1..];
        let hours = self.number(2, 2)?;
        let minutes = match !self.eat(b':') && self.0.is_empty() {
            true => 0,
            false => self.number(2, 2)?,
        };
        (self.0.is_empty() && hours < 24 && minutes < 60)
            .then_some(sign * (hours * 3600 + minutes * 60))
    }
}

fn written(seconds: i64, east: i64) -> Option<(i64, String)> {
    let sign = if east < 0 { '-' } else { '+' };
    let minutes = east.abs() / 60;
    let zone = format!("{sign}{:02}{:02}", minutes / 60, minutes % 60);
    (seconds >= 0).then_some((seconds, zone))
}

/// Git's own form. Without the `@`, a number below nine digits is not
/// taken for seconds, as git does not take it.
fn raw(text: &str) -> Option<(i64, String)> {
    let mut at = Cursor(text.as_bytes());
    let marked = at.eat(b'@');
    let seconds = at.number(1, 18)?;
    if !marked && seconds < 100_000_000 {
        return None;
    }
    at.spaces();
    written(seconds, at.zone(Some(0))?)
}

/// The days from the epoch to a day of the Gregorian calendar.
fn days(year: i64, month: i64, day: i64) -> Option<i64> {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let length = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    if day < 1 || day > length {
        return None;
    }
    // the year begins in March, so that the leap day ends it
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn at_zone(day: i64, time: i64, rest: Cursor) -> Option<(i64, String)> {
    let east = rest.zone(Some(0))?;
    written(day * 86_400 + time - east, east)
}

/// `YYYY-MM-DD`, a `T` or a space, the time, and a zone or none.
fn iso(text: &str) -> Option<(i64, String)> {
    let mut at = Cursor(text.as_bytes());
    let year = at.number(4, 4)?;
    let month = at.eat(b'-').then(|| at.number(2, 2))??;
    let day = at.eat(b'-').then(|| at.number(2, 2))??;
    if !at.eat(b'T') && !at.spaces() {
        return None;
    }
    let time = at.time()?;
    at.spaces();
    at_zone(days(year, month, day)?, time, at)
}

/// `Thu, 07 Apr 2005 22:13:13 +0200`; the day of the week may be left
/// out and is not checked against the date.
fn rfc2822(text: &str) -> Option<(i64, String)> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let text = match text.split_once(", ") {
        Some((weekday, rest)) if weekday.len() == 3 => rest,
        _ => text,
    };
    let mut at = Cursor(text.as_bytes());
    let day = at.number(1, 2)?;
    at.spaces().then_some(())?;
    let named = at.0.get(..3)?;
    let month = MONTHS.iter().position(|m| m.as_bytes() == named)? as i64 + 1;
    at.0 = &at.0[3..];
    at.spaces().then_some(())?;
    let year = at.number(4, 4)?;
    at.spaces().then_some(())?;
    let time = at.time()?;
    at.spaces();
    at_zone(days(year, month, day)?, time, at)
}

#[cfg(test)]
mod tests {
    use super::parse;

    /// Each date and what git 2.56.1 wrote for it, run in the zone UTC.
    #[test]
    fn dates_are_read_as_git_reads_them() {
        for (given, seconds, zone) in [
            ("1700000000 +0900", 1_700_000_000, "+0900"),
            ("@1700000000 -0330", 1_700_000_000, "-0330"),
            ("1700000000", 1_700_000_000, "+0000"),
            ("100000000 +0000", 100_000_000, "+0000"),
            ("2005-04-07T22:13:13+02:00", 1_112_904_793, "+0200"),
            ("2005-04-07T22:13:13Z", 1_112_911_993, "+0000"),
            ("2005-04-07 22:13:13 +0200", 1_112_904_793, "+0200"),
            ("2005-04-07T22:13:13", 1_112_911_993, "+0000"),
            ("Thu, 07 Apr 2005 22:13:13 +0200", 1_112_904_793, "+0200"),
            ("2005-04-07T22:13:13.123+0200", 1_112_904_793, "+0200"),
            ("2005-04-07T22:13:13+02", 1_112_904_793, "+0200"),
            ("2005-04-07T22:13+0200", 1_112_904_780, "+0200"),
        ] {
            assert_eq!(parse(given), Some((seconds, zone.to_owned())), "{given}");
        }
        for refused in [
            "2005-04-07",
            "yesterday",
            "garbage",
            "99999999 +0000",
            "12345 +0100",
        ] {
            assert_eq!(parse(refused), None, "{refused}");
        }
    }

    #[test]
    fn a_date_the_calendar_does_not_have_is_refused() {
        assert_eq!(parse("2023-02-29T00:00:00Z"), None);
        assert_eq!(
            parse("2024-02-29T00:00:00Z"),
            Some((1_709_164_800, "+0000".into()))
        );
        assert_eq!(parse("2005-13-01T00:00:00Z"), None);
        assert_eq!(parse("2005-04-07T24:00:00Z"), None);
        assert_eq!(parse("2005-04-07T22:13:13+0260"), None);
        assert_eq!(parse("1969-12-31T23:59:59Z"), None);
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some((0, "+0000".into())));
    }
}
