//! `charset::decode` against a table recorded from the WHATWG decoders.

use alt_cli::charset;

use crate::patterns::unhex;

#[test]
fn decoding_matches_the_recorded_table() {
    let table = include_str!("../fixtures/charset/decode.txt");
    let mut cases = 0;
    for line in table.lines().filter(|l| !l.starts_with('#')) {
        let mut fields = line.split('\t');
        let label = unhex(fields.next().unwrap());
        let input = unhex(fields.next().unwrap());
        let want = match fields.next().unwrap() {
            "none" => None,
            hex => Some(unhex(hex)),
        };
        assert_eq!(charset::decode(&label, &input), want, "{line}");
        cases += 1;
    }
    assert_eq!(cases, 1851);
}
