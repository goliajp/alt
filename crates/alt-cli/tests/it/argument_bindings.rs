//! The canonical argument value bound into durable request records must not
//! drift: a retry after an upgrade has to reproduce the stored binding.
//! The fixture pairs a command line with the value recorded for it.

use std::ffi::OsString;

#[test]
fn bindings_match_the_recorded_ones() {
    let fixture = include_str!("../fixtures/args/bindings.txt");
    let mut lines = fixture.lines();
    let mut checked = 0;
    while let (Some(command), Some(expected)) = (lines.next(), lines.next()) {
        let args = command.split(' ').map(OsString::from).collect();
        let parsed = alt_cli::machine::parse(args)
            .unwrap_or_else(|r| panic!("{command}: {}", String::from_utf8_lossy(&r.stderr)));
        let actual = alt_json::to_string(&parsed.normalized).unwrap();
        assert_eq!(actual, expected, "{command}");
        checked += 1;
    }
    assert_eq!(checked, 223);
}
