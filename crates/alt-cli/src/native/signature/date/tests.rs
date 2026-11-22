use alt_os::zone::Zone;

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

/// The dates of [`GIT`] git reads and this does not: a time or a zone
/// that cannot be one, which git passes over, and a day the month does
/// not have or no day, which git counts on from the month's first.
const STRICTER: [&str; 14] = [
    "2005-04-07 22:13:13 +2400",
    "2005-04-07 25:13:13 +0200",
    "2005-04-07 24:00:00 +0000",
    "2005-02-30 22:13:13 +0200",
    "2005-04-31 22:13:13 +0200",
    "Apr 2005 22:13:13 +0000",
    "2005-04-07T22:13:13+0260",
    "2005-04-07T22:13:13+02:60",
    "2005-04-07T22:13:13+2:00",
    "2005-04-07T22:13:13+020",
    "2005-04-07 22:13:13 -3",
    "2005-04-07 22:13:13 GMT+9",
    "2005-04-07 22:13:13 +0900 -3",
    "@1700000000 +9999",
];

fn read(given: &str, tz: &str) -> Option<String> {
    let zone = Zone::of(Some(tz));
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
        for (tz, written) in ZONES.iter().zip(written) {
            let both = written != "ERR" && !STRICTER.contains(&given);
            let expected = both.then(|| written.to_owned());
            assert_eq!(read(given, tz), expected, "{given:?} in {tz}");
            read_by_both += usize::from(both);
        }
    }
    assert_eq!(rows().count(), 172);
    assert_eq!(read_by_both, (172 - 31 - STRICTER.len()) * ZONES.len());
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
        ("Nowhere/Zone", "2005-04-07 22:13:13", "1112911993 +0000"),
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
