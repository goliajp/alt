//! Tiny HTTP front-end that proxies to `git upload-pack` /
//! `git receive-pack` against a local repo. Shared by the M6/W4 (fetch)
//! and M6/W5 (push) integration tests so the wire-side coverage runs
//! against real git binaries without bringing in a full HTTP server crate.
//!
//! The shim is intentionally minimal — Content-Length-framed requests,
//! `HTTP/1.0` + `Connection: close` responses, and the `Git-Protocol`
//! header is propagated to the subprocess as the `GIT_PROTOCOL` env var
//! (the same path `git http-backend` itself uses). That's enough for ureq
//! (alt-wire-http's client) and for switching `upload-pack` into v2.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// Spawn the test HTTP server bound to a free port on 127.0.0.1, returning
/// the URL prefix. The listener thread is leaked; the OS cleans it up when
/// the test process exits.
pub fn spawn(repo_dir: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let repo_dir = repo_dir.clone();
            thread::spawn(move || {
                let _ = handle_one(stream, &repo_dir);
            });
        }
    });
    url
}

fn handle_one(mut stream: std::net::TcpStream, repo: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let parts: Vec<&str> = line.trim_end().split(' ').collect();
    if parts.len() < 3 {
        return Ok(());
    }
    let method = parts[0].to_owned();
    let url = parts[1].to_owned();

    let mut content_length: usize = 0;
    let mut git_protocol: Option<String> = None;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let trimmed = header.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if let Some(v) = lower.strip_prefix("git-protocol:") {
            git_protocol = Some(v.trim().to_string());
        }
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    let (status, payload, ct) = dispatch(&method, &url, repo, &git_protocol, &body)?;

    let resp_head = format!(
        "HTTP/1.0 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(resp_head.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

fn dispatch(
    method: &str,
    url: &str,
    repo: &Path,
    git_protocol: &Option<String>,
    body: &[u8],
) -> std::io::Result<(&'static str, Vec<u8>, &'static str)> {
    // `/info/refs?service=git-upload-pack` and `/git-upload-pack` for fetch;
    // `/info/refs?service=git-receive-pack` and `/git-receive-pack` for
    // push. Both endpoints proxy to the matching git subprocess.
    if method == "GET" && url.starts_with("/info/refs") {
        let (service, subcommand) = if url.contains("git-receive-pack") {
            ("git-receive-pack", "receive-pack")
        } else {
            ("git-upload-pack", "upload-pack")
        };
        let mut cmd = Command::new("git");
        cmd.args([subcommand, "--http-backend-info-refs"]);
        cmd.arg(repo);
        if let Some(p) = git_protocol {
            cmd.env("GIT_PROTOCOL", p);
        }
        let out = cmd.output()?;
        assert!(
            out.status.success(),
            "{service} info-refs failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut body_out = Vec::new();
        write_pkt(&mut body_out, format!("# service={service}\n").as_bytes());
        body_out.extend_from_slice(b"0000");
        body_out.extend_from_slice(&out.stdout);
        Ok((
            "200 OK",
            body_out,
            // a small leak in &'static str gymnastics: the two
            // advertisement Content-Types only differ in their middle
            // segment, so we match on `service` to pick one
            if service == "git-receive-pack" {
                "application/x-git-receive-pack-advertisement"
            } else {
                "application/x-git-upload-pack-advertisement"
            },
        ))
    } else if method == "POST" && (url == "/git-upload-pack" || url == "/git-receive-pack") {
        let (service, subcommand) = if url == "/git-receive-pack" {
            ("git-receive-pack", "receive-pack")
        } else {
            ("git-upload-pack", "upload-pack")
        };
        let mut cmd = Command::new("git");
        cmd.args([subcommand, "--stateless-rpc"]);
        cmd.arg(repo);
        if let Some(p) = git_protocol {
            cmd.env("GIT_PROTOCOL", p);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        child.stdin.as_mut().unwrap().write_all(body)?;
        drop(child.stdin.take());
        let out = child.wait_with_output()?;
        assert!(
            out.status.success(),
            "{service} stateless-rpc failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let ct = if service == "git-receive-pack" {
            "application/x-git-receive-pack-result"
        } else {
            "application/x-git-upload-pack-result"
        };
        Ok(("200 OK", out.stdout, ct))
    } else {
        Ok(("404 Not Found", Vec::new(), "text/plain"))
    }
}

fn write_pkt(out: &mut Vec<u8>, payload: &[u8]) {
    let total = payload.len() + 4;
    out.extend_from_slice(format!("{total:04x}").as_bytes());
    out.extend_from_slice(payload);
}
