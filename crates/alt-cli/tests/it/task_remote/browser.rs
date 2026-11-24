//! The service as a browser asks it: with the cookie of a session, and the token
//! against forgery where it changes something.
use super::*;

fn browser_request(
    server: &Server,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Option<Value>,
) -> (u16, String, Value) {
    use std::io::Write;
    let headers = alt_os::temp::NamedTempFile::new().unwrap();
    let output = alt_os::temp::NamedTempFile::new().unwrap();
    let mut command = Command::new("curl");
    command
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "60",
            "--dump-header",
        ])
        .arg(headers.path())
        .arg("--output")
        .arg(output.path())
        .args(["--write-out", "%{http_code}", "--header"])
        .arg(format!("Origin: {}", server.url));
    if let Some(cookie) = cookie {
        command.arg("--header").arg(format!("Cookie: {cookie}"));
    }
    if let Some(csrf) = csrf {
        command.arg("--header").arg(format!("X-Alt-CSRF: {csrf}"));
    }
    if body.is_some() {
        command.args([
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
        ]);
    }
    let mut child = command
        .arg(format!("{}{path}", server.url))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(body) = body {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&alt_json::to_vec(&body).unwrap())
            .unwrap();
    } else {
        drop(child.stdin.take());
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "browser request failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    (
        std::str::from_utf8(&result.stdout)
            .unwrap()
            .parse()
            .unwrap(),
        std::fs::read_to_string(headers.path()).unwrap(),
        alt_json::from_reader(std::fs::File::open(output.path()).unwrap()).unwrap(),
    )
}
pub(super) fn browser_login(server: &Server) -> (String, Value) {
    let (status, headers, value) = browser_request(
        server,
        "/web/login",
        None,
        None,
        Some(json!({"login_name":"reviewer","password":"a private browser acceptance password"})),
    );
    assert_eq!(status, 200, "{value}");
    let cookie = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
                .map(|(_, value)| value.trim().split(';').next().unwrap().to_owned())
        })
        .unwrap();
    (cookie, value)
}
pub(super) fn browser_json(
    server: &Server,
    path: &str,
    cookie: &str,
    csrf: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, _, value) = browser_request(server, path, Some(cookie), csrf, body);
    (status, value)
}
