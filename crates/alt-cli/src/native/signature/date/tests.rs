use alt_os::zone::Zone;

use super::super::test_zones::zone;
use super::parse;

/// 2026-11-23, the day git was asked: a date more than ten days past it
/// is not read month first.
const NOW: i64 = 1_795_374_000;

const ZONES: [&str; 3] = ["UTC", "Asia/Tokyo", "America/New_York"];

/// Each date and the seconds and the zone git wrote for it as
/// `GIT_AUTHOR_DATE` with `TZ` set to each of [`ZONES`], `ERR` where it
/// refused the date. Git 2.47.3 on Linux with glibc and git 2.56.1 on
/// macOS wrote the same for every one of them.
const GIT: &str = include_str!("git_dates.tsv");

/// The dates of [`GIT`] git reads and this does not: a time that cannot
/// be one, which git takes for other numbers or counts on into the next
/// day; a zone that cannot be one beside no zone in numbers, which git
/// passes over, so that it reads `GMT+9` as GMT; and a day the month does
/// not have or no day, which git counts on from the month's first.
const STRICTER: [&str; 25] = [
    "2005-04-07 25:13:13 +0200",
    "2005-04-07 24:30:00 +0000",
    "2005-04-07 24:00:01 +0000",
    "2005-04-07 24:00:60 +0000",
    "2005-04-07T2430 +0000",
    "2005-04-07T2401 +0000",
    "243000 20050407 +0000",
    "2005-04-07 22:13:13 +2400",
    "2005-04-07T22:13:13+0260",
    "2005-04-07T22:13:13+02:60",
    "2005-04-07T22:13:13+2:00",
    "2005-04-07T22:13:13+020",
    "2005-04-07 22:13:13 -3",
    "@1700000000 +9999",
    "2005-04-07 22:13:13 GMT+9",
    "2005-04-07 22:13:13 UTC+9",
    "2005-04-07 22:13:13 JST-9",
    "2005-04-07T22:13:13Z-3",
    "2005-04-07 22:13:13 +2400 UTC",
    "2005-04-07 22:13:13 UTC +2400",
    "2005-04-07 22:13:13 +2400 EST",
    "2005-04-07 22:13:13 EST +2400",
    "2005-02-30 22:13:13 +0200",
    "2005-04-31 22:13:13 +0200",
    "Apr 2005 22:13:13 +0000",
];

/// The dates of [`GIT`] this reads otherwise than git, and what it
/// reads: a minute west of UTC is that zone, where git, which marks a
/// date without a zone by that number, reads the date in the caller's.
const OTHERWISE: [(&str, &str); 2] = [
    ("2005-04-07 22:13:13 -0001", "1112912053 -0001"),
    ("2005-04-07 22:13:13 -00:01", "1112912053 -0001"),
];

/// What `given` is read as by a caller whose `TZ` is `tz`: one of
/// [`ZONES`] or another the tests have a file of, by its file, and
/// anything else as it is spelled.
fn read(given: &str, tz: &str) -> Option<String> {
    let spelled;
    let zone = match zone(tz) {
        Some(zone) => zone,
        None => {
            spelled = Zone::of(Some(tz));
            &spelled
        }
    };
    let read = parse(given, NOW, &|wall| zone.offset_of_local(wall));
    read.map(|(seconds, zone)| format!("{seconds} {zone}"))
}

fn rows() -> impl Iterator<Item = (&'static str, Vec<&'static str>)> {
    GIT.lines().map(|row| {
        let mut cells = row.split('\t');
        (cells.next().unwrap(), cells.collect())
    })
}

#[test]
fn dates_are_read_as_git_reads_them() {
    let mut read_by_both = 0;
    for (given, written) in rows() {
        assert_eq!(written.len(), ZONES.len(), "{given:?}");
        let otherwise = OTHERWISE.iter().find(|(of, _)| *of == given);
        for (tz, written) in ZONES.iter().zip(written) {
            let both = written != "ERR" && !STRICTER.contains(&given) && otherwise.is_none();
            let expected = match otherwise {
                Some((_, here)) => Some((*here).to_owned()),
                None => both.then(|| written.to_owned()),
            };
            assert_eq!(read(given, tz), expected, "{given:?} in {tz}");
            read_by_both += usize::from(both);
        }
    }
    assert_eq!(rows().count(), 218);
    // the dates git refuses, in each zone it refuses them in
    let refused = rows().flat_map(|(_, written)| written);
    assert_eq!(refused.filter(|written| *written == "ERR").count(), 103);
    let apart = 103 + (STRICTER.len() + OTHERWISE.len()) * ZONES.len();
    assert_eq!(read_by_both, 218 * ZONES.len() - apart);
}

/// A zone that cannot be one is passed over beside one in numbers, as
/// git passes it over, and not beside a zone by name alone.
#[test]
fn a_zone_that_is_none_is_passed_over_only_beside_one_in_numbers() {
    for (given, written) in [
        ("2005-04-07 22:13:13 +0200 (UTC+2)", "1112904793 +0200"),
        ("2005-04-07 22:13:13 +0200 foo-123", "1112904793 +0200"),
        ("2005-04-07 22:13:13 0200 -3", "1112904793 +0200"),
        ("2005-04-07 22:13:13 +0900 +9", "1112879593 +0900"),
        ("2005-04-07 22:13:13 -3 +0900", "1112879593 +0900"),
        ("2005-04-07 22:13:13 +0900 -3", "1112879593 +0900"),
        ("2005-04-07 22:13:13 JST +0900 -3", "1112879593 +0900"),
    ] {
        assert_eq!(read(given, "UTC").as_deref(), Some(written), "{given}");
    }
    for given in [
        "GMT+9",
        "UTC+9",
        "JST-9",
        "Z-3",
        "+2400 EST",
        "EST +2400",
        "-3 Z",
    ] {
        let given = format!("2005-04-07 22:13:13 {given}");
        assert_eq!(read(&given, "UTC"), None, "{given}");
    }
}

/// What git wrote for each: the last second it reads is the last of
/// 2099 where the time is, and a time west of UTC that ends past it is
/// refused, though none is that has passed it before its zone is counted.
#[test]
fn the_last_moment_read_is_the_end_of_the_last_year_where_the_time_is() {
    for (given, tz, written) in [
        ("2099-12-31 22:59:59 -0100", "UTC", Some("4102444799 -0100")),
        ("2099-12-31 23:00:00 -0100", "UTC", None),
        ("2099-12-31 23:59:59 -0100", "UTC", None),
        ("2099-12-31 23:59:60 -0100", "UTC", Some("4102448400 -0100")),
        ("2099-12-31 24:00:00 -0100", "UTC", Some("4102448400 -0100")),
        ("2099-12-31 24:00:00 +0100", "UTC", Some("4102441200 +0100")),
        (
            "2099-12-31 18:59:59",
            "America/New_York",
            Some("4102444799 -0500"),
        ),
        ("2099-12-31 19:00:00", "America/New_York", None),
        ("2099-12-31 23:59:59", "America/New_York", None),
        (
            "2099-12-31 24:00:00",
            "America/New_York",
            Some("4102462800 -0500"),
        ),
        (
            "2099-12-31 23:59:59",
            "Asia/Tokyo",
            Some("4102412399 +0900"),
        ),
        ("1970-01-01 09:00:00", "Asia/Tokyo", Some("0 +0900")),
        ("1970-01-01 08:59:59", "Asia/Tokyo", None),
        (
            "1970-01-01 00:00:00",
            "America/New_York",
            Some("18000 -0500"),
        ),
    ] {
        assert_eq!(read(given, tz).as_deref(), written, "{given} in {tz}");
    }
}

/// What git wrote for each: midnight written as the end of a day is the
/// first moment of the next, in every form a time has.
#[test]
fn the_twenty_fourth_hour_is_the_first_moment_of_the_next_day() {
    for given in [
        "2005-04-07 24:00:00 +0000",
        "2005-04-07 24:00 +0000",
        "2005-04-07T24:00:00Z",
        "240000 20050407 +0000",
        "20050407T240000 +0000",
        "2005-04-07T24 +0000",
        "2005-04-07T2400 +0000",
    ] {
        assert_eq!(read(given, "UTC").as_deref(), Some("1112918400 +0000"));
    }
    let last = read("2099-12-31 24:00:00 +0000", "UTC");
    assert_eq!(last.as_deref(), Some("4102444800 +0000"));
}

/// A date of blanks alone is no date, as it is none to git; one of
/// nothing at all is not given here, being taken for not set.
#[test]
fn blanks_are_no_date() {
    for given in ["", " ", "\t", "  \t ", "\n", " \n2005-04-07 22:13:13 +0200"] {
        assert_eq!(read(given, "UTC"), None, "{given:?}");
    }
}

/// A step of a generator that gives the same numbers every run.
fn next(state: &mut u64) -> usize {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 11) as usize
}

#[test]
fn no_text_makes_reading_a_date_fail() {
    const LETTERS: &[u8] = b"0123456789 :-+/.,@TZAPMaprJSUGtu\n9 0:";
    let zones = ZONES.map(|tz| zone(tz).unwrap());
    let mut state = 0xD1B5_4A32_D192_ED03;
    let mut texts: Vec<Vec<u8>> = Vec::new();
    for _ in 0..6000 {
        let length = next(&mut state) % 48;
        let made = (0..length).map(|_| LETTERS[next(&mut state) % LETTERS.len()]);
        texts.push(made.collect());
    }
    for (given, _) in rows() {
        let given = given.as_bytes();
        for at in 0..given.len() {
            texts.push(given[..at].to_vec());
            for _ in 0..3 {
                let mut changed = given.to_vec();
                changed[at] = LETTERS[next(&mut state) % LETTERS.len()];
                texts.push(changed);
            }
        }
        // numbers of every length where the date has its first
        for length in [1, 9, 18, 19, 20, 40] {
            let mut longer = vec![b'9'; length];
            longer.extend(given);
            texts.push(longer);
        }
    }
    // seconds near the last that can be counted, and a time after them
    for seconds in ["9223372036854775806", "9223372036854775000"] {
        for rest in ["", " 23:59:59", " 24:00", " 2099-12-31 23:59:60", " +2359"] {
            texts.push(format!("{seconds}{rest}").into_bytes());
        }
    }
    assert!(texts.len() > 20_000);
    let mut dated = 0;
    for (number, text) in texts.iter().enumerate() {
        let text = String::from_utf8(text.clone()).unwrap();
        let zone = &zones[number % zones.len()];
        let Some((seconds, zone)) = parse(&text, NOW, &|wall| zone.offset_of_local(wall)) else {
            continue;
        };
        dated += 1;
        // seconds written as a commit carries them are whatever they are
        let last = if text.starts_with('@') {
            i64::MAX
        } else {
            4_102_531_200
        };
        assert!((0..=last).contains(&seconds), "{text:?}: {seconds}");
        let (sign, digits) = zone.split_at(1);
        let digits = digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit());
        assert!(digits && matches!(sign, "+" | "-"), "{text:?}: {zone}");
    }
    assert!(
        dated * 100 >= texts.len() * 10,
        "{dated} of {}",
        texts.len()
    );
}

#[test]
fn a_time_or_a_zone_that_cannot_be_one_and_a_day_the_month_lacks_are_refused() {
    for given in STRICTER {
        let (_, written) = rows().find(|(of, _)| *of == given).expect(given);
        assert!(!written.contains(&"ERR"), "git reads {given:?}");
        for tz in ZONES {
            assert_eq!(read(given, tz), None, "{given:?} in {tz}");
        }
    }
    assert_eq!(read("2023-02-29T00:00:00Z", "UTC"), None);
    let leap = read("2024-02-29T00:00:00Z", "UTC");
    assert_eq!(leap.as_deref(), Some("1709164800 +0000"));
}

#[test]
fn a_date_ends_at_the_end_of_its_line() {
    let two = read("2005-04-07 22:13:13 +0200\n2006 +0900", "UTC");
    assert_eq!(two.as_deref(), Some("1112904793 +0200"));
}

/// What git wrote under glibc with `TZ` set to each of these.
#[test]
fn a_date_without_a_zone_is_in_the_zone_tz_spells() {
    for (tz, given, written) in [
        ("JST-9", "2005-04-07 22:13:13", "1112879593 +0900"),
        ("<+0330>-3:30", "2005-04-07 22:13:13", "1112899393 +0330"),
        (
            "EST5EDT,M3.2.0,M11.1.0",
            "2005-01-07 22:13:13",
            "1105153993 -0500",
        ),
        (
            "EST5EDT,M3.2.0,M11.1.0",
            "2005-07-07 22:13:13",
            "1120788793 -0400",
        ),
        ("EST5EDT,M3.2.0,M11.1.0", "1105135993", "1105135993 -0500"),
        (
            "AEST-10AEDT,M10.1.0,M4.1.0/3",
            "2005-01-07 22:13:13",
            "1105096393 +1100",
        ),
        (
            "AEST-10AEDT,M10.1.0,M4.1.0/3",
            "2090-07-01 12:00:00",
            "3802557600 +1000",
        ),
        (
            "<-03>3<-02>,M3.5.0/-2,M10.5.0/-1",
            "2005-07-07 22:13:13",
            "1120781593 -0200",
        ),
        (
            "IST-2IDT,M3.4.4/26,M10.5.0",
            "2005-01-07 22:13:13",
            "1105128793 +0200",
        ),
        (":Asia/Tokyo", "1112904793", "1112904793 +0900"),
        ("Asia/Kathmandu", "1971-06-01 12:00:00", "44605800 +0530"),
        ("Europe/Dublin", "2005-07-07 22:13:13", "1120770793 +0100"),
        (
            "America/New_York",
            "2090-01-01 12:00:00",
            "3786973200 -0500",
        ),
        // the zone was 44 minutes and 30 seconds west, and git drops the
        // seconds from the zone and from the instant alike
        ("Africa/Monrovia", "1971-06-01 12:00:00", "44628240 -0044"),
        ("", "2005-04-07 22:13:13", "1112911993 +0000"),
    ] {
        assert_eq!(read(given, tz).as_deref(), Some(written), "{given} in {tz}");
    }
}

#[test]
fn the_zone_of_the_caller_is_not_asked_for_where_the_date_names_one() {
    let unasked = |_| panic!("the date names its zone");
    let read = parse("2005-04-07T22:13:13+02:00", NOW, &unasked);
    assert_eq!(read, Some((1_112_904_793, "+0200".to_owned())));
    let read = parse("Thu Apr 7 20:13:13 UTC 2005", NOW, &unasked);
    assert_eq!(read, Some((1_112_904_793, "+0000".to_owned())));
}
