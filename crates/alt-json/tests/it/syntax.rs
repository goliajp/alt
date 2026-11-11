use crate::check::{parse, report};

/// nesting: 127 levels are accepted, 128 are not, for arrays, objects and
/// mixtures, closed or cut short
#[test]
fn depth_limit() {
    let (mut cases, mut accepted) = (0, 0);
    for depth in (1..=5).chain(120..=135).chain([200, 1000, 100_000]) {
        let arrays = "[".repeat(depth);
        let objects = "{\"a\":".repeat(depth);
        let mixed = "[{\"a\":".repeat(depth / 2 + 1);
        let texts = [
            format!("{arrays}{}", "]".repeat(depth)),
            format!("{arrays}1{}", "]".repeat(depth)),
            arrays.clone(),
            format!("{arrays}1"),
            format!("{objects}1{}", "}".repeat(depth)),
            format!("{objects}{{}}{}", "}".repeat(depth)),
            objects.clone(),
            format!("{mixed}0{}", "}]".repeat(depth / 2 + 1)),
            format!(" {arrays} [ ] {}", "]".repeat(depth)),
        ];
        for text in texts {
            let ok = parse(text.as_bytes());
            accepted += u64::from(ok);
            cases += 1;
        }
    }
    assert!(alt_json::parse(format!("{}{}", "[".repeat(127), "]".repeat(127)).as_bytes()).is_ok());
    let error = alt_json::parse(format!("{}{}", "[".repeat(128), "]".repeat(128)).as_bytes());
    assert_eq!(
        error.unwrap_err().to_string(),
        "recursion limit exceeded at line 1 column 128"
    );
    report("syntax, depth limit", cases, Some(accepted));
}

#[test]
fn listed_documents() {
    let texts: &[&[u8]] = &[
        b"",
        b" ",
        b"\n",
        b"\t\r\n ",
        b"null",
        b" null ",
        b"\nnull\n",
        b"nul",
        b"nulL",
        b"nulll",
        b"null null",
        b"null,",
        b"n",
        b"N",
        b"Null",
        b"true",
        b"tru",
        b"truE",
        b"truee",
        b"false",
        b"fals",
        b"falsE",
        b"t",
        b"f",
        b"True",
        b"FALSE",
        b"[",
        b"]",
        b"{",
        b"}",
        b"[]",
        b"{}",
        b"[ ]",
        b"{ }",
        b"[\n]",
        b"[,]",
        b"[1,]",
        b"[,1]",
        b"[1,,2]",
        b"[1 2]",
        b"[1,2",
        b"[1,",
        b"[1",
        b"[1:2]",
        b"[1;2]",
        b"[1],",
        b"[1]]",
        b"[[1]",
        b"[1,2,]",
        b"[ 1 , 2 ]",
        b"[1,\n]",
        b"{,}",
        b"{\"a\"}",
        b"{\"a\":}",
        b"{\"a\":1,}",
        b"{\"a\":1,,}",
        b"{,\"a\":1}",
        b"{\"a\" 1}",
        b"{\"a\":1 \"b\":2}",
        b"{\"a\":1,\"b\"}",
        b"{\"a\":1,\"b\":}",
        b"{\"a\":1",
        b"{\"a\":",
        b"{\"a\"",
        b"{\"a",
        b"{\"",
        b"{\"a\":1,",
        b"{\"a\":1, ",
        b"{a:1}",
        b"{'a':1}",
        b"{1:1}",
        b"{null:1}",
        b"{[]:1}",
        b"{\"a\":1,2:3}",
        b"{\"a\":1,}",
        b"{\"a\":1}}",
        b"{\"a\"::1}",
        b"{\"a\":1:2}",
        b"{\"a\"=1}",
        b"{ \"a\" : 1 , \"b\" : 2 }",
        b"{\"a\":1,\n}",
        b"{\"a\":1 ,\t\"b\" :\r2}",
        b"\"",
        b"\"a",
        b"\"a\"",
        b"\"a\"\"",
        b"\"a\" \"b\"",
        b"'a'",
        b"a",
        b"\"\\",
        b"\"\\\"",
        b"\"\\u",
        b"\"\\u0",
        b"\"\\u00",
        b"\"\\u004",
        b"\"\\u0041",
        b"\"\\u0041\"",
        b"\"\n\"",
        b"\"\t\"",
        b"\"\r\"",
        b"\"\x00\"",
        b"\"\x7f\"",
        b"\"a\nb\"",
        b"\"line\\nbreak\"",
        b"1 2",
        b"1,2",
        b"[1]x",
        b"[1] x",
        b"[1]\n\n\nx",
        b"\n\n[1,\n\n2,\n x]",
        b"[1,\n2,\n\"a\nb\"]",
        b"{\"a\":\n\n\n}",
        b"\n\n\n",
        b"\xef\xbb\xbfnull",
        b"\xef\xbb\xbf",
        b"null\xef\xbb\xbf",
        b"\x00",
        b"null\x00",
        b"[\x00]",
        b"\x0b1",
        b"\x0c1",
        b"\xa01",
        b"1\x0b",
        b"1\x0c",
        b"/* c */ 1",
        b"1 // c",
        b"# c\n1",
        b"[1 /* c */]",
        b"undefined",
        b"NaN",
        b"Infinity",
        b"-Infinity",
        b"+1",
        b"-",
        b"--1",
        b"-a",
        b"- 1",
        b"[-]",
        b"[-,1]",
        b".",
        b"1.",
        b".1",
        b"1.e1",
        b"0x1",
        b"1e",
        b"[1e]",
        b"[1e,]",
        b"[1.,2]",
        b"[01]",
        b"[-01]",
        b"[00]",
        b"[0 0]",
        b"tRue",
        b"[t]",
        b"[n]",
        b"[f]",
        b"[nul]",
        b"[tru,1]",
        b"{\"a\":nul}",
        b"{\"a\":tru",
        b"[\"a\",]",
        b"[\"\\x\"]",
        b"[\"\\ud800\"]",
        b"[\"\\udc00\"]",
        b"[\"\\ud800\\ud800\"]",
        b"[\"\\ud800\\u0041\"]",
        b"{\"\\ud800\":1}",
        b"{\"a\\",
        b"{\"a\\u12",
        b"{\"a\x01\":1}",
        b"{\"\xff\":1}",
        b"[\"\xff\"]",
        b"[\"\xc3\"]",
        b"{\"a\":\"\xff\"}",
        b"\"\xff\"x",
        b"\"\xff",
        b"\"\xff\\",
        b"\"\xff\\q\"",
        b"\"\xff\x01\"",
    ];
    let mut accepted = 0;
    for text in texts {
        accepted += u64::from(parse(text));
    }
    report(
        "syntax, listed documents",
        texts.len() as u64,
        Some(accepted),
    );
}

/// every pair and every triple from a small alphabet of structural bytes
#[test]
fn all_short_documents() {
    let alphabet = b"[]{}:,\"\\ \n01-.eEtrufalsn\x00\x1f\x7f\xc3\xa9/+";
    let (mut cases, mut accepted) = (0, 0);
    for &a in alphabet {
        accepted += u64::from(parse(&[a]));
        cases += 1;
        for &b in alphabet {
            accepted += u64::from(parse(&[a, b]));
            cases += 1;
            for &c in alphabet {
                accepted += u64::from(parse(&[a, b, c]));
                cases += 1;
                for &d in alphabet {
                    accepted += u64::from(parse(&[a, b, c, d]));
                    cases += 1;
                }
            }
        }
    }
    report("syntax, all short documents", cases, Some(accepted));
}
