//! Request dispatch is part of the wire contract. Every request below is sent
//! as raw bytes and the whole answer is compared with the recorded one: status
//! line, every header in order (the date and trace values are masked) and the
//! body. `ALT_SERVER_RECORD_DISPATCH=1` rewrites the record.
use super::*;
use std::io::{Read, Write};
mod cases;
use cases::{browser_cases, cloud_cases, native_cases};

const RECORD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/it/fixtures/http-dispatch.txt"
);
const ORIGIN: &str = "https://workbench.example.test";

pub(super) struct Case {
    label: String,
    request: Vec<u8>,
}
/// `$TOKEN` and `$BASIC` in header values stand for the fixture's credential
pub(super) fn case(method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Case {
    let mut label = if target.len() > 120 {
        format!("{method} {}... ({} bytes)", &target[..40], target.len())
    } else {
        format!("{method} {target}")
    };
    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: alt.example.test\r\n");
    for (name, value) in headers {
        label.push_str(&format!(" [{name}: {value}]"));
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT") {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("Connection: close\r\n\r\n");
    if !body.is_empty() {
        label.push_str(&format!(" body={}", body.escape_ascii()));
    }
    let mut request = request.into_bytes();
    request.extend_from_slice(body);
    Case { label, request }
}
fn replace(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some(at) = rest.windows(from.len()).position(|w| w == from) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(to);
        rest = &rest[at + from.len()..];
    }
    out.extend_from_slice(rest);
    out
}
/// the answer as recorded, and whether a handler produced it
fn exchange(server: &Running, token: &str, case: &Case) -> (String, bool) {
    let basic = alt_base64::STANDARD.encode(format!("alt:{token}"));
    let request = replace(&case.request, b"$TOKEN", token.as_bytes());
    let request = replace(&request, b"$BASIC", basic.as_bytes());
    let mut socket =
        std::net::TcpStream::connect(server.url.strip_prefix("http://").unwrap()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    socket.write_all(&request).unwrap();
    let mut raw = Vec::new();
    socket.read_to_end(&mut raw).unwrap();
    let mut out = String::new();
    let mut rest = &raw[..];
    let mut traced = false;
    loop {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap_or_else(|| panic!("{}: {}", case.label, raw.escape_ascii()));
        let head = std::str::from_utf8(&rest[..end]).unwrap();
        rest = &rest[end + 4..];
        for line in head.split("\r\n") {
            let masked = ["date", "x-alt-trace-id"].iter().find(|name| {
                line.split_once(':')
                    .is_some_and(|(found, _)| found.eq_ignore_ascii_case(name))
            });
            match masked {
                Some(name) => {
                    traced |= *name == "x-alt-trace-id";
                    out.push_str(&format!("{}: <masked>\n", &line[..name.len()]));
                }
                None => {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        out.push('\n');
        if !head.starts_with("HTTP/1.1 100 ") {
            break;
        }
    }
    out.push_str(&rest.escape_ascii().to_string());
    out.push('\n');
    (out, traced)
}
/// the per-route response counters after `handled` responses have ended
fn counters(server: &Running, token: &str, handled: u64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let text = server.metrics(token);
        let mut lines: Vec<_> = text
            .lines()
            .filter(|line| {
                (line.starts_with("alt_http_responses_total{")
                    || line.starts_with("alt_http_response_bytes_total{"))
                    && !line.contains("route=\"metrics\"")
            })
            .collect();
        lines.sort_unstable();
        let ended: u64 = lines
            .iter()
            .filter(|line| line.starts_with("alt_http_responses_total{"))
            .map(|line| line.rsplit(' ').next().unwrap().parse::<u64>().unwrap())
            .sum();
        if ended == handled {
            return lines.join("\n");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{ended} of {handled} responses ended: {text}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn run(name: &str, server: &Running, token: &str, cases: &[Case], scrape: bool) -> String {
    let mut out = String::new();
    let mut handled = 0;
    for case in cases {
        let (answer, traced) = exchange(server, token, case);
        handled += u64::from(traced);
        out.push_str(&format!(">> {name} | {}\n{answer}\n", case.label));
    }
    if scrape {
        out.push_str(&format!(
            ">> {name} | counters\n{}\n\n",
            counters(server, token, handled)
        ));
    }
    out
}

fn assets(f: &Fixture) -> alt_server::WebAssets {
    let root = f.root.path().join("assets");
    for (path, bytes) in [
        ("index.html", "<!doctype html><title>alt</title>"),
        ("app.js", "export {};\n"),
        ("v1/shadow.js", "shadow\n"),
        ("git/repository/style.css", "body{}\n"),
        ("web/inside.js", "inside\n"),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    alt_server::WebAssets::load(&root).unwrap()
}
fn cloud(f: &mut Fixture) -> Running {
    use golia_platform_contract::{Audience, Counter, Product, Secret};
    let audience = Audience {
        organization_id: "organization".to_string().try_into().unwrap(),
        workspace_id: f.spec.workspace_id.clone().try_into().unwrap(),
        installation_id: f.spec.installation_id.clone().try_into().unwrap(),
        product: Product::Alt,
        generation: Counter(1),
        origin: ORIGIN.to_string().try_into().unwrap(),
        control_origin: "http://127.0.0.1:9".to_string().try_into().unwrap(),
    };
    let key: Secret = "ab".repeat(32).try_into().unwrap();
    f.client()
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .unwrap();
    f.spec.deployment_mode = DeploymentMode::Cloud;
    let runtime =
        std::sync::Arc::new(alt_server::cloud::Runtime::new(audience.clone(), key).unwrap());
    f.store = Store::initialize_cloud(
        f.client(),
        ByteStore::open(&f.root.path().join("bytes")).unwrap(),
        f.spec.clone(),
        SigningKey::from_bytes(&[7; 32]),
        limits(),
        &audience,
        runtime.key_digest(),
    )
    .unwrap();
    let web = alt_server::WebOptions {
        origin: alt_browser_auth::PublicOrigin::parse(ORIGIN).unwrap(),
        assets: assets(f),
        session_ttl_ms: 60_000,
    };
    Running::start_with_cloud(f, Some(web), Some(runtime))
}

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; compares every dispatch outcome with its record"]
fn every_request_shape_gets_its_recorded_status_headers_and_body() {
    let mut observed = String::new();
    {
        let f = Fixture::new();
        let server = Running::start(&f);
        observed.push_str(&run("native", &server, &f.token, &native_cases(), true));
    }
    {
        let mut f = Fixture::new();
        let origin = alt_browser_auth::PublicOrigin::parse(ORIGIN).unwrap();
        f.store.enable_browser(&f.credential, &origin).unwrap();
        let web = alt_server::WebOptions {
            origin,
            assets: assets(&f),
            session_ttl_ms: 60_000,
        };
        let server = Running::start_with_browser(&f, Some(web));
        observed.push_str(&run("browser", &server, &f.token, &browser_cases(), true));
    }
    {
        let mut f = Fixture::new();
        let server = cloud(&mut f);
        observed.push_str(&run("cloud", &server, &f.token, &cloud_cases(), false));
    }
    if std::env::var_os("ALT_SERVER_RECORD_DISPATCH").is_some() {
        std::fs::write(RECORD, &observed).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(RECORD).unwrap();
    let mut differing = Vec::new();
    let (mut old, mut new) = (recorded.split(">> "), observed.split(">> "));
    loop {
        match (old.next(), new.next()) {
            (None, None) => break,
            (a, b) if a == b => (),
            (a, b) => differing.push(format!(
                "recorded:\n{}\nobserved:\n{}",
                a.unwrap_or("(nothing)"),
                b.unwrap_or("(nothing)")
            )),
        }
    }
    assert!(
        differing.is_empty(),
        "{} of the recorded answers changed\n{}",
        differing.len(),
        differing.join("\n")
    );
}
