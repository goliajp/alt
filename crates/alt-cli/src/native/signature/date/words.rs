//! The words of a date: the months and the days of the week by their
//! English names or the first three letters or more of them, the zones
//! git knows by name, `AM` and `PM`, and the `T` between a date and a
//! time.

use super::Read;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const WEEKDAYS: [&str; 7] = [
    "Sundays",
    "Mondays",
    "Tuesdays",
    "Wednesdays",
    "Thursdays",
    "Fridays",
    "Saturdays",
];

/// The names git has for zones, the hours each is east of UTC, and
/// whether it is a summer time, which is an hour further east.
const ZONES: &[(&str, i64, bool)] = &[
    ("IDLW", -12, false),
    ("NT", -11, false),
    ("CAT", -10, false),
    ("HST", -10, false),
    ("HDT", -10, true),
    ("YST", -9, false),
    ("YDT", -9, true),
    ("PST", -8, false),
    ("PDT", -8, true),
    ("MST", -7, false),
    ("MDT", -7, true),
    ("CST", -6, false),
    ("CDT", -6, true),
    ("EST", -5, false),
    ("EDT", -5, true),
    ("AST", -3, false),
    ("ADT", -3, true),
    ("WAT", -1, false),
    ("GMT", 0, false),
    ("UTC", 0, false),
    ("Z", 0, false),
    ("WET", 0, false),
    ("BST", 0, true),
    ("CET", 1, false),
    ("MET", 1, false),
    ("MEWT", 1, false),
    ("MEST", 1, true),
    ("CEST", 1, true),
    ("MESZ", 1, true),
    ("FWT", 1, false),
    ("FST", 1, true),
    ("EET", 2, false),
    ("EEST", 2, true),
    ("WAST", 7, false),
    ("WADT", 7, true),
    ("CCT", 8, false),
    ("JST", 9, false),
    ("EAST", 10, false),
    ("EADT", 10, true),
    ("GST", 10, false),
    ("NZT", 12, false),
    ("NZST", 12, false),
    ("NZDT", 12, true),
    ("IDLE", 12, false),
];

/// How many bytes `text` begins with of `name`, whatever their case,
/// where a letter or a digit that is not of the name does not follow
/// them; 0 where one does.
fn begins(text: &[u8], name: &str) -> usize {
    let name = name.as_bytes();
    let pairs = text.iter().zip(name);
    let length = pairs
        .take_while(|(byte, of)| byte.eq_ignore_ascii_case(of))
        .count();
    match text.get(length) {
        Some(next) if next.is_ascii_alphanumeric() => 0,
        _ => length,
    }
}

impl Read {
    /// The word `text` begins with: how much of `text` it is.
    pub(super) fn word(&mut self, text: &[u8]) -> usize {
        let named = |names: &[&str]| {
            let lengths = names.iter().map(|name| begins(text, name));
            lengths.enumerate().find(|(_, length)| *length >= 3)
        };
        if let Some((month, length)) = named(&MONTHS) {
            self.month = Some(month as i64 + 1);
            return length;
        }
        if let Some((_, length)) = named(&WEEKDAYS) {
            return length;
        }
        for (name, hours, summer) in ZONES {
            let length = begins(text, name);
            if length >= 3 || length == name.len() {
                // a zone given in numbers stands before one given by name
                if self.zone.is_none() {
                    self.zone = Some(60 * (hours + i64::from(*summer)));
                }
                return length;
            }
        }
        if begins(text, "PM") == 2 {
            // the hour git takes where none is read yet is the eleventh
            self.hour = Some(self.hour.unwrap_or(-1) % 12 + 12);
            return 2;
        }
        if begins(text, "AM") == 2 {
            self.hour = self.hour.map(|hour| hour % 12);
            return 2;
        }
        if text[0] == b'T' && super::digit_at(text, 1) && self.hour.is_none() {
            // hours alone, or hours and minutes, may follow
            (self.minute, self.second) = (Some(0), Some(0));
            return 1;
        }
        text.iter().take_while(|b| b.is_ascii_alphabetic()).count()
    }
}
