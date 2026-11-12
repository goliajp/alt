//! Times URL parsing, with ASCII and with international host names, and
//! form-urlencoded parsing; prints the fastest of several rounds, in
//! nanoseconds per URL and per query string.
use alt_url::{Url, form};
use std::hint::black_box;
use std::time::Instant;

const URLS: &[&str] = &[
    "https://alt.golia.jp",
    "https://alt.golia.jp/",
    "http://127.0.0.1:8080",
    "http://localhost:3000",
    "http://[::1]:8080/",
    "https://control.example.com/cloud/access",
    "https://control.example.com/cloud/access?installationId=0123456789abcdef&challenge=AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_AbCdEfG&state=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "https://user:pass@Example.COM:443/a/b/../c/./d?x=1&y=2#frag",
    "http://192.168.0.1/path/to/resource.html",
    "https://example.org/a%20b/c d?q=1 2#f g",
];

const INTERNATIONAL: &[&str] = &[
    "https://\u{4f8b}\u{3048}.jp",
    "https://b\u{fc}cher.example/",
    "https://\u{43f}\u{440}\u{438}\u{43c}\u{435}\u{440}.\u{440}\u{444}/",
    "https://\u{645}\u{62b}\u{627}\u{644}.\u{625}\u{62e}\u{62a}\u{628}\u{627}\u{631}",
    "https://e\u{301}cole.fr",
    "https://%E4%BE%8B%E3%81%88.jp/path",
];

const QUERY: &[u8] = b"installationId=0123456789abcdef&challenge=AbCdEfGhIjKl%2BMnOp+QrSt&state=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

fn fastest(rounds: usize, iterations: usize, mut run: impl FnMut()) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..rounds {
        let start = Instant::now();
        for _ in 0..iterations {
            run();
        }
        best = best.min(start.elapsed().as_nanos() as f64 / iterations as f64);
    }
    best
}

fn main() {
    let parse = fastest(5, 200_000, || {
        for input in URLS {
            black_box(Url::parse(black_box(input)).ok());
        }
    }) / URLS.len() as f64;
    let pairs = fastest(5, 500_000, || {
        for pair in form::parse(black_box(QUERY)) {
            black_box(pair);
        }
    });
    let international = fastest(5, 100_000, || {
        for input in INTERNATIONAL {
            black_box(Url::parse(black_box(input)).ok());
        }
    }) / INTERNATIONAL.len() as f64;
    println!(
        "parse {parse:.1} ns/url, form {pairs:.1} ns/query, non-ASCII host {international:.1} ns/url"
    );
}
