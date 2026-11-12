//! The hand-written matchers against a table recorded from the regular
//! expressions they replace.

use alt_cli::patterns;

pub fn unhex(hex: &str) -> Vec<u8> {
    if hex == "\"\"" {
        return Vec::new();
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn text(hex: &str) -> String {
    String::from_utf8(unhex(hex)).unwrap()
}

#[test]
fn matchers_agree_with_the_recorded_table() {
    let table = include_str!("../fixtures/patterns/matches.txt");
    let mut cases = 0;
    for row in table.lines().filter(|l| !l.starts_with('#')) {
        let fields: Vec<&str> = row.split('\t').collect();
        let line = text(fields[1]);
        let want = (fields[2] != "none").then(|| text(fields[2]));
        let find = match fields[0] {
            "conventional" => {
                let got = patterns::conventional(&line)
                    .map(|c| (c.kind.to_owned(), c.breaking, c.summary.to_owned()));
                let want = want.map(|kind| (kind, fields[3] == "1", text(fields[4])));
                assert_eq!(got, want, "{row}");
                cases += 1;
                continue;
            }
            "private-key" => patterns::private_key,
            "cloud-access-key" => patterns::cloud_access_key,
            "forge-token" => patterns::forge_token,
            "sk-api-key" => patterns::sk_api_key,
            "chat-token" => patterns::chat_token,
            "browser-api-key" => patterns::browser_api_key,
            "payment-live-key" => patterns::payment_live_key,
            "secret-assignment" => patterns::assignment_value,
            other => panic!("unknown pattern {other}"),
        };
        assert_eq!(find(&line), want.as_deref(), "{row}");
        cases += 1;
    }
    assert_eq!(cases, 1624);
}

#[test]
fn the_first_shape_in_order_wins() {
    use patterns::Shape;
    type Finder = fn(&str) -> Option<&str>;
    let shapes: [(Shape, Finder); 7] = [
        (Shape::PrivateKey, patterns::private_key),
        (Shape::CloudAccessKey, patterns::cloud_access_key),
        (Shape::ForgeToken, patterns::forge_token),
        (Shape::SkApiKey, patterns::sk_api_key),
        (Shape::ChatToken, patterns::chat_token),
        (Shape::BrowserApiKey, patterns::browser_api_key),
        (Shape::PaymentLiveKey, patterns::payment_live_key),
    ];
    let table = include_str!("../fixtures/patterns/matches.txt");
    let lines: Vec<String> = (table.lines())
        .filter(|l| !l.starts_with('#'))
        .map(|row| text(row.split('\t').nth(1).unwrap()))
        .collect();
    let mut found = 0;
    // pairs of lines too, so that one line holds several shapes
    for (i, line) in lines.iter().enumerate() {
        for line in [
            line.clone(),
            format!("{line} {}", lines[(i * 7 + 3) % lines.len()]),
        ] {
            let want = (shapes.iter()).find_map(|(shape, find)| Some((*shape, find(&line)?)));
            assert_eq!(patterns::credential(&line), want, "{line:?}");
            found += want.is_some() as usize;
        }
    }
    assert!(found > 800, "{found}");
}
