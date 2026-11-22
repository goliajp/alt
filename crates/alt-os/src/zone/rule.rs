//! A zone as POSIX spells one in `TZ`, and as a zone file's last line
//! does for the years after its table: `std offset [dst [offset]
//! [,start[/time],end[/time]]]`, as in `JST-9` or
//! `EST5EDT,M3.2.0,M11.1.0`. An offset is hours west of UTC.

use super::{DAY, civil_from_days, days_from_civil, days_in_month, is_leap};

/// A day of every year.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Day {
    /// `Jn`: the nth day of a year that is taken to have no February 29
    Julian(i64),
    /// `n`: the days after January 1, February 29 counted
    Zero(i64),
    /// `Mm.w.d`: weekday `d` of week `w` of month `m`; week 5 is the last
    Month { month: i64, week: i64, weekday: i64 },
}

impl Day {
    /// The days from the epoch to this day of `year`.
    fn of(self, year: i64) -> i64 {
        let new_year = days_from_civil(year, 1, 1);
        match self {
            Day::Julian(nth) => new_year + nth - 1 + i64::from(is_leap(year) && nth >= 60),
            Day::Zero(after) => new_year + after,
            Day::Month {
                month,
                week,
                weekday,
            } => {
                let first = days_from_civil(year, month, 1);
                // the epoch was a Thursday
                let of_first = (first + 4).rem_euclid(7);
                let day = (weekday - of_first).rem_euclid(7) + 7 * (week - 1);
                let last = days_in_month(year, month) - 1;
                first + if day > last { day - 7 } else { day }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Summer {
    /// seconds east of UTC while it lasts
    offset: i32,
    /// the day and the second of that day, by the clock that then holds,
    /// it begins on and ends on
    start: (Day, i64),
    end: (Day, i64),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Rule {
    /// seconds east of UTC
    standard: i32,
    summer: Option<Summer>,
}

impl Rule {
    pub(super) fn standard(&self) -> i32 {
        self.standard
    }

    pub(super) fn offset_at(&self, instant: i64) -> i32 {
        let Some(summer) = &self.summer else {
            return self.standard;
        };
        let start =
            |year| summer.start.0.of(year) * DAY + summer.start.1 - i64::from(self.standard);
        let end = |year| summer.end.0.of(year) * DAY + summer.end.1 - i64::from(summer.offset);
        let (this, _, _) = civil_from_days(instant.div_euclid(DAY));
        let lasts = (this - 1..=this + 1).any(|year| {
            let begun = start(year);
            // south of the equator it ends in the year after
            let over = match end(year) {
                over if over > begun => over,
                _ => end(year + 1),
            };
            (begun..over).contains(&instant)
        });
        if lasts { summer.offset } else { self.standard }
    }
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

    fn number(&mut self) -> Option<i64> {
        let digits = self.0.iter().take_while(|b| b.is_ascii_digit()).count();
        let (number, rest) = self.0.split_at(digits);
        self.0 = rest;
        std::str::from_utf8(number).ok()?.parse().ok()
    }

    /// `<...>` around letters, digits and signs, or three letters or more.
    fn name(&mut self) -> Option<()> {
        if self.eat(b'<') {
            let within = |b: &&u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-');
            let length = self.0.iter().take_while(within).count();
            self.0 = &self.0[length..];
            return self.eat(b'>').then_some(());
        }
        let length = self
            .0
            .iter()
            .take_while(|b| b.is_ascii_alphabetic())
            .count();
        self.0 = &self.0[length..];
        (length >= 3).then_some(())
    }

    /// `[+-]hh[:mm[:ss]]` in seconds; the hours may be as many as a week.
    fn span(&mut self) -> Option<i64> {
        let sign = if self.eat(b'-') { -1 } else { 1 };
        if sign == 1 {
            self.eat(b'+');
        }
        let hours = self.number()?;
        let mut rest = [0, 0];
        for part in &mut rest {
            if !self.eat(b':') {
                break;
            }
            *part = self.number()?;
        }
        let [minutes, seconds] = rest;
        (hours <= 167 && minutes < 60 && seconds < 60)
            .then_some(sign * (hours * 3600 + minutes * 60 + seconds))
    }

    /// A day and the time on it a change is made at, which is two in the
    /// morning where none is given.
    fn change(&mut self) -> Option<(Day, i64)> {
        let day = if self.eat(b'J') {
            let nth = self.number()?;
            (1..=365).contains(&nth).then_some(Day::Julian(nth))?
        } else if self.eat(b'M') {
            let month = self.number()?;
            let week = self.eat(b'.').then(|| self.number())??;
            let weekday = self.eat(b'.').then(|| self.number())??;
            let known =
                (1..=12).contains(&month) && (1..=5).contains(&week) && (0..=6).contains(&weekday);
            known.then_some(Day::Month {
                month,
                week,
                weekday,
            })?
        } else {
            let after = self.number()?;
            (0..=365).contains(&after).then_some(Day::Zero(after))?
        };
        let time = match self.eat(b'/') {
            true => self.span()?,
            false => 7200,
        };
        Some((day, time))
    }
}

/// When summer time begins and ends where a rule names it and says no
/// more: the second Sunday of March and the first of November.
const UNSAID: &[u8] = b",M3.2.0,M11.1.0";

pub(super) fn parse(text: &str) -> Option<Rule> {
    let mut at = Cursor(text.as_bytes());
    at.name()?;
    let standard = -at.span()?;
    let offset = |seconds: i64| i32::try_from(seconds).ok();
    if at.0.is_empty() {
        return Some(Rule {
            standard: offset(standard)?,
            summer: None,
        });
    }
    at.name()?;
    let summer = match at.0.first() {
        None | Some(b',') => standard + 3600,
        Some(_) => -at.span()?,
    };
    if at.0.is_empty() {
        at = Cursor(UNSAID);
    }
    let start = at.eat(b',').then(|| at.change())??;
    let end = at.eat(b',').then(|| at.change())??;
    at.0.is_empty().then_some(Rule {
        standard: offset(standard)?,
        summer: Some(Summer {
            offset: offset(summer)?,
            start,
            end,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> i64 {
        days_from_civil(year, month, day) * DAY + hour * 3600 + minute * 60
    }

    #[test]
    fn a_zone_without_summer_time_has_one_offset() {
        for (text, east) in [
            ("JST-9", 32_400),
            ("EST5", -18_000),
            ("EST+5", -18_000),
            ("<+0330>-3:30", 12_600),
            ("<-03>3", -10_800),
            ("UTC0", 0),
            ("XXX-5:45:30", 20_730),
        ] {
            let rule = parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(rule.offset_at(at(2005, 4, 7, 0, 0)), east, "{text}");
            assert_eq!(rule.offset_at(at(2005, 12, 7, 0, 0)), east, "{text}");
        }
    }

    #[test]
    fn what_is_not_a_rule_is_refused() {
        for text in [
            "",
            "JS-9",
            "JST",
            "Asia/Tokyo",
            "JST-9x",
            "EST5EDT,M3.2.0",
            "EST5EDT,M13.2.0,M11.1.0",
            "EST5EDT,M3.6.0,M11.1.0",
            "EST5EDT,M3.2.7,M11.1.0",
            "EST5EDT,J0,J300",
            "EST5EDT,J366,J300",
            "EST5EDT,366,300",
            "EST5EDT,M3.2.0,M11.1.0,",
            "EST5:60",
            "EST168",
            "<EST5",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    /// The instants either side of each change, in UTC.
    #[test]
    fn summer_time_lasts_between_the_days_the_rule_names() {
        let us = parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        // 2005-03-13 02:00 EST and 2005-11-06 02:00 EDT
        assert_eq!(us.offset_at(at(2005, 3, 13, 6, 59)), -18_000);
        assert_eq!(us.offset_at(at(2005, 3, 13, 7, 0)), -14_400);
        assert_eq!(us.offset_at(at(2005, 11, 6, 5, 59)), -14_400);
        assert_eq!(us.offset_at(at(2005, 11, 6, 6, 0)), -18_000);
        assert_eq!(parse("EST5EDT").unwrap(), us);
        assert_eq!(parse("EST5EDT4,M3.2.0/2,M11.1.0/2:00:00").unwrap(), us);

        // the last Sunday of March and of October, at 01:00 UTC
        let europe = parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        assert_eq!(europe.offset_at(at(2005, 3, 27, 0, 59)), 3600);
        assert_eq!(europe.offset_at(at(2005, 3, 27, 1, 0)), 7200);
        assert_eq!(europe.offset_at(at(2005, 10, 30, 0, 59)), 7200);
        assert_eq!(europe.offset_at(at(2005, 10, 30, 1, 0)), 3600);
    }

    #[test]
    fn summer_time_south_of_the_equator_spans_the_new_year() {
        let sydney = parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(sydney.offset_at(at(2005, 1, 7, 0, 0)), 39_600);
        assert_eq!(sydney.offset_at(at(2005, 7, 7, 0, 0)), 36_000);
        assert_eq!(sydney.offset_at(at(2005, 12, 31, 23, 59)), 39_600);
        assert_eq!(sydney.offset_at(at(1970, 1, 1, 0, 0)), 39_600);
        // 2005-04-03 03:00 AEDT and 2005-10-02 02:00 AEST
        assert_eq!(sydney.offset_at(at(2005, 4, 2, 15, 59)), 39_600);
        assert_eq!(sydney.offset_at(at(2005, 4, 2, 16, 0)), 36_000);
        assert_eq!(sydney.offset_at(at(2005, 10, 1, 15, 59)), 36_000);
        assert_eq!(sydney.offset_at(at(2005, 10, 1, 16, 0)), 39_600);
    }

    #[test]
    fn days_are_counted_with_and_without_the_leap_day() {
        // J60 is March 1 in every year, 59 is February 29 in a leap year
        let julian = parse("XYZ5ABC4:30,J60/1,J300/3:30").unwrap();
        assert_eq!(julian.offset_at(at(2004, 3, 1, 5, 59)), -18_000);
        assert_eq!(julian.offset_at(at(2004, 3, 1, 6, 0)), -16_200);
        assert_eq!(julian.offset_at(at(2005, 3, 1, 6, 0)), -16_200);
        let zero = parse("XYZ5ABC,59,299").unwrap();
        assert_eq!(zero.offset_at(at(2004, 2, 29, 7, 0)), -14_400);
        assert_eq!(zero.offset_at(at(2004, 2, 29, 6, 59)), -18_000);
        assert_eq!(zero.offset_at(at(2005, 3, 1, 7, 0)), -14_400);
        assert_eq!(zero.offset_at(at(2005, 3, 1, 6, 59)), -18_000);
    }

    #[test]
    fn a_change_may_be_made_before_its_day_begins_or_after_it_ends() {
        // the Saturday before the last Sunday of March at 22:00, and the
        // one before the last of October at 23:00
        let greenland = parse("<-03>3<-02>,M3.5.0/-2,M10.5.0/-1").unwrap();
        assert_eq!(greenland.offset_at(at(2005, 3, 27, 0, 59)), -10_800);
        assert_eq!(greenland.offset_at(at(2005, 3, 27, 1, 0)), -7200);
        assert_eq!(greenland.offset_at(at(2005, 10, 30, 0, 59)), -7200);
        assert_eq!(greenland.offset_at(at(2005, 10, 30, 1, 0)), -10_800);
        // the Friday after the fourth Thursday of March, at 02:00
        let israel = parse("IST-2IDT,M3.4.4/26,M10.5.0").unwrap();
        assert_eq!(israel.offset_at(at(2005, 3, 24, 23, 59)), 7200);
        assert_eq!(israel.offset_at(at(2005, 3, 25, 0, 0)), 10_800);
        // summer time the whole year
        let always = parse("EST5EDT,0/0,J365/25").unwrap();
        for month in 1..=12 {
            assert_eq!(always.offset_at(at(2005, month, 1, 0, 0)), -14_400);
            assert_eq!(always.offset_at(at(2005, month, 28, 23, 0)), -14_400);
        }
    }
}
