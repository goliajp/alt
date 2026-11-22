//! The date `GIT_AUTHOR_DATE` or `GIT_COMMITTER_DATE` gives, read by the
//! rules git reads it by. The text is taken a token at a time, in any
//! order: a word is a month, a day of the week, a zone by its name, `AM`
//! or `PM`; a number is seconds since the epoch, a date, a time, a day, a
//! year or a month by its shape and by what is already known; a sign
//! before digits is a zone. Whatever is none of these is passed over. A
//! date without a zone is in the caller's, as a moment that is given no
//! date at all is written in the caller's zone.
//!
//! Three things git lets through are refused: a time written as one that
//! cannot be one (`25:13:13`, `24:30:00`; `24:00:00` is the next day's
//! first moment), a zone written as one that cannot be one (`+2400`,
//! `-3`) where the date has no other, and a day its month does not have,
//! or none. And a zone of `-0001` is that zone, where git takes it for
//! no zone.

use alt_os::zone::{civil_from_days, days_from_civil, days_in_month};

mod words;

const DAY: i64 = 86_400;
/// the last year git reads
const LAST_YEAR: i64 = 2099;

/// What has been read of a date so far.
#[derive(Default)]
struct Read {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    hour: Option<i64>,
    minute: Option<i64>,
    second: Option<i64>,
    /// minutes east of UTC
    zone: Option<i64>,
    /// the date and the time came of seconds since the epoch, which are
    /// in no zone
    absolute: bool,
    /// a time was written that cannot be one
    refused: bool,
    /// a zone was written that cannot be one
    unshaped: bool,
}

fn digit_at(text: &[u8], at: usize) -> bool {
    text.get(at).is_some_and(u8::is_ascii_digit)
}

/// The number whose digits begin at `at` and where they end.
fn digits(text: &[u8], at: usize) -> (i64, usize) {
    let length = text[at..].iter().take_while(|b| b.is_ascii_digit()).count();
    let end = at + length;
    let number = std::str::from_utf8(&text[at..end]).ok();
    let number = number.and_then(|digits| digits.parse().ok());
    (number.unwrap_or(i64::MAX), end)
}

/// The seconds from the epoch to a time of a day in UTC, for the years
/// git reads.
fn seconds(year: i64, month: i64, day: i64, time: [Option<i64>; 3]) -> Option<i64> {
    let [hour, minute, second] = time;
    if !(1970..=LAST_YEAR).contains(&year) {
        return None;
    }
    let day = days_from_civil(year, month, 1) + day - 1;
    Some(day * DAY + hour? * 3600 + minute? * 60 + second?)
}

impl Read {
    fn nothing(&self) -> bool {
        let Read {
            year,
            month,
            day,
            hour,
            minute,
            second,
            ..
        } = self;
        [year, month, day, hour, minute, second]
            .iter()
            .all(|part| part.is_none())
    }

    fn time(&self) -> [Option<i64>; 3] {
        [self.hour, self.minute, self.second]
    }

    fn set_time(&mut self, hour: i64, minute: i64, second: i64) -> bool {
        // the day ends at 24:00:00 and has no later time
        let is_one = match hour {
            0..24 => (0..60).contains(&minute) && (0..=60).contains(&second),
            24 => minute == 0 && second == 0,
            _ => false,
        };
        if is_one {
            (self.hour, self.minute, self.second) = (Some(hour), Some(minute), Some(second));
        }
        is_one
    }

    /// Takes a year of four digits or of two, a month and a day for the
    /// date where they can be one; `year` is -1 where none was written.
    /// With `not_after`, the date is not taken either where it lies more
    /// than ten days past that instant, and one without a year is taken.
    fn set_date(&mut self, year: i64, month: i64, day: i64, not_after: Option<i64>) -> bool {
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return false;
        }
        if not_after.is_none() {
            // git keeps these two of a date whose year it then refuses
            (self.month, self.day) = (Some(month), Some(day));
        }
        let full = match (year, not_after) {
            (-1, Some(now)) => civil_from_days(now.div_euclid(DAY)).0,
            (1970..=LAST_YEAR, _) => year,
            (71..=99, _) => 1900 + year,
            (0..=37, _) => 2000 + year,
            _ => return false,
        };
        if let Some(now) = not_after
            && seconds(full, month, day, self.time()).is_some_and(|then| now + 10 * DAY < then)
        {
            return false;
        }
        (self.month, self.day) = (Some(month), Some(day));
        if year != -1 {
            self.year = Some(full);
        }
        true
    }

    /// Two or three numbers with `between` between them, the first of
    /// which is `first` and ends at `end`: a time or a date. How much of
    /// `text` they are, or 0 where they are neither.
    fn several(&mut self, first: i64, between: u8, text: &[u8], end: usize, now: i64) -> usize {
        let (second, mut end) = digits(text, end + 1);
        let mut third = -1;
        if text.get(end) == Some(&between) && digit_at(text, end + 1) {
            (third, end) = digits(text, end + 1);
        }
        if between == b':' {
            if !self.set_time(first, second, third.max(0)) {
                self.refused = true;
                return 0;
            }
            // a fraction of a second, which is dropped
            let dated = self.year.is_some() && self.month.is_some() && self.day.is_some();
            if dated && text.get(end) == Some(&b'.') && digit_at(text, end + 1) {
                end = digits(text, end + 1).1;
            }
            return end;
        }
        let now = Some(now);
        let dotted = between == b'.';
        // year-month-day, year-day-month, month/day/year, day.month.year
        // and month.day.year, in the order git tries them
        let is_date = first > 70
            && (self.set_date(first, second, third, None)
                || self.set_date(first, third, second, None))
            || !dotted && self.set_date(third, first, second, now)
            || self.set_date(third, second, first, now)
            || dotted && self.set_date(third, first, second, now);
        if is_date { end } else { 0 }
    }

    /// The number `text` begins with: how much of `text` it is.
    fn number(&mut self, text: &[u8], now: i64) -> usize {
        let (number, end) = digits(text, 0);
        if (100_000_000..i64::MAX).contains(&number) && self.nothing() {
            let (year, month, day) = civil_from_days(number / DAY);
            let of_day = number % DAY;
            (self.year, self.month, self.day) = (Some(year), Some(month), Some(day));
            self.set_time(of_day / 3600, of_day / 60 % 60, of_day % 60);
            self.absolute = true;
            return end;
        }
        let between = text.get(end).copied().unwrap_or(0);
        if matches!(between, b':' | b'.' | b'/' | b'-') && digit_at(text, end + 1) {
            let both = self.several(number, between, text, end, now);
            if both > 0 {
                return both;
            }
        }
        let (high, middle, low) = (number / 10_000, number / 100 % 100, number % 100);
        match end {
            // the date or the time without anything between its parts
            8 => {
                self.set_date(high, middle, low, None);
                return end;
            }
            6 => {
                let timed = self.set_time(high, middle, low);
                if timed && text.get(end) == Some(&b'.') && digit_at(text, end + 1) {
                    return digits(text, end + 1).1;
                }
                return end;
            }
            _ => {}
        }
        // the hours, or the hours and the minutes, after a `T`
        if self.hour.is_none() && self.minute == Some(0) && self.second == Some(0) {
            let (hour, minute) = if end == 4 { (middle, low) } else { (number, 0) };
            if matches!(end, 2 | 4) && self.set_time(hour, minute, 0) {
                return end;
            }
            (self.minute, self.second) = (None, None);
        }
        if end == 4 {
            if number <= 1400 && self.zone.is_none() {
                self.zone = Some(middle * 60 + low);
            } else if (1901..2100).contains(&number) {
                self.year = Some(number);
            }
            return end;
        }
        if end > 2 {
            return end;
        }
        // a day before a month or a year
        if (1..32).contains(&number) && self.day.is_none() {
            self.day = Some(number);
        } else if end == 2 && self.year.is_none() && number < 10 && self.day.is_some() {
            self.year = Some(2000 + number);
        } else if end == 2 && self.year.is_none() && number >= 70 {
            self.year = Some(1900 + number);
        } else if (1..13).contains(&number) && self.month.is_none() {
            self.month = Some(number);
        }
        end
    }

    /// The sign `text` begins with and the digits after it, `hh`, `hhmm`
    /// or `hh:mm`: how much of `text` they are.
    fn zone(&mut self, text: &[u8]) -> usize {
        let (mut hours, mut end) = digits(text, 1);
        let mut minutes = 0;
        let mut shaped = true;
        match end - 1 {
            4 => (hours, minutes) = (hours / 100, hours % 100),
            2 if text.get(end) == Some(&b':') => {
                (minutes, end) = digits(text, end + 1);
                shaped = end == 6;
            }
            2 => {}
            _ => shaped = false,
        }
        if shaped && hours < 24 && minutes < 60 {
            let east = hours * 60 + minutes;
            self.zone = Some(if text[0] == b'-' { -east } else { east });
        } else {
            self.unshaped = true;
        }
        end
    }
}

/// A zone `east` minutes east of UTC as `±hhmm`.
pub(super) fn zone(east: i64) -> String {
    let sign = if east < 0 { '-' } else { '+' };
    let minutes = east.abs();
    format!("{sign}{:02}{:02}", minutes / 60, minutes % 60)
}

fn written(seconds: i64, east: i64) -> (i64, String) {
    (seconds, zone(east))
}

/// `@<seconds> <zone>` and nothing else, as a commit carries a date: the
/// one form in which seconds of fewer than nine digits are read. The
/// seconds, the sign of the zone and its four digits.
fn of_commit(text: &[u8]) -> Option<(i64, i64, i64)> {
    let text = text.strip_prefix(b"@")?;
    let (seconds, end) = digits(text, 0);
    let sign = match text.get(end..end + 2)? {
        b" +" => 1,
        b" -" => -1,
        _ => return None,
    };
    let (zone, last) = digits(text, end + 2);
    let ends = matches!(text.get(last), None | Some(b'\n'));
    (end > 0 && seconds < i64::MAX && last == end + 6 && ends).then_some((seconds, sign, zone))
}

/// The seconds since the epoch and the zone as `±hhmm`. `now` is the
/// instant a date far in the future is told by, and `local` gives the
/// seconds the caller's zone is east of UTC by where its clock reads the
/// seconds given, counted as in UTC.
pub(super) fn parse(text: &str, now: i64, local: &dyn Fn(i64) -> i32) -> Option<(i64, String)> {
    let text = text.as_bytes();
    if let Some((seconds, sign, zone)) = of_commit(text) {
        let (hours, minutes) = (zone / 100, zone % 100);
        return (hours < 24 && minutes < 60)
            .then(|| written(seconds, sign * (hours * 60 + minutes)));
    }
    let mut read = Read::default();
    let mut at = 0;
    while let Some(first) = text.get(at).filter(|byte| **byte != b'\n') {
        let rest = &text[at..];
        let taken = match first {
            b'a'..=b'z' | b'A'..=b'Z' => read.word(rest),
            b'0'..=b'9' => read.number(rest, now),
            b'+' | b'-' if digit_at(rest, 1) => read.zone(rest),
            _ => 0,
        };
        at += taken.max(1);
    }
    let (year, month, day) = (read.year?, read.month?, read.day?);
    let unzoned = read.unshaped && read.zone.is_none();
    if read.refused || unzoned || day > days_in_month(year, month) {
        return None;
    }
    let wall = seconds(year, month, day, read.time())?;
    let east = match read.zone {
        Some(east) => east,
        None => i64::from(local(wall)) / 60,
    };
    if read.absolute {
        return Some(written(wall, east));
    }
    // 2099-12-31T23:59:59Z, past which git reads nothing
    let last = days_from_civil(LAST_YEAR + 1, 1, 1) * DAY - 1;
    let before_the_first = east > 0 && east * 60 > wall;
    let past_the_last = east < 0 && -east * 60 > last - wall;
    (!before_the_first && !past_the_last).then(|| written(wall - east * 60, east))
}

#[cfg(test)]
mod tests;
