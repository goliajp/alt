//! Tiny HTTP front-end that proxies to `git upload-pack` /
//! `git receive-pack` against a local repo. Shared by the fetch
//! and push integration tests so the wire-side coverage runs
//! against real git binaries without bringing in a full HTTP server crate.
//!
//! The shim is intentionally minimal — Content-Length-framed requests of
//! HTTP/1.1, responses in chunks with `Connection: close`, and the
//! `Git-Protocol` header is propagated to the subprocess as the
//! `GIT_PROTOCOL` env var (the same path `git http-backend` itself uses).
//! That's enough for ureq (alt-wire-http's client) and for switching
//! `upload-pack` into v2.
//!
//! Like `git http-backend`, it sends the status line and the headers before
//! it runs the service, and passes the service's output on as it comes. The
//! last chunk goes out once the service ended well: a client reads the answer
//! of one that failed, even after its whole answer, as cut short. A client
//! that goes away has the service's output closed, and the service ends.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

mod ended;
mod held;

/// Spawn the test HTTP server bound to a free port on 127.0.0.1, returning
/// the URL prefix. The listener thread is leaked; the OS cleans it up when
/// the test process exits.
pub fn spawn(repo_dir: PathBuf) -> String {
    spawn_with_env(repo_dir, Vec::new())
}

/// [`spawn`], with `env` set on every git process the server runs.
pub fn spawn_with_env(repo_dir: PathBuf, env: Vec<(String, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (repo_dir, env) = (repo_dir.clone(), env.clone());
            thread::spawn(move || {
                let _ = handle_one(stream, &repo_dir, &env);
            });
        }
    });
    url
}

fn handle_one(mut stream: TcpStream, repo: &Path, env: &[(String, String)]) -> std::io::Result<()> {
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

    let push = url.contains("git-receive-pack");
    let (service, subcommand) = if push {
        ("git-receive-pack", "receive-pack")
    } else {
        ("git-upload-pack", "upload-pack")
    };
    let mut cmd = Command::new("git");
    cmd.envs(env.iter().map(|(k, v)| (k, v)));
    if let Some(p) = &git_protocol {
        cmd.env("GIT_PROTOCOL", p);
    }
    // `/info/refs?service=git-upload-pack` and `/git-upload-pack` for fetch;
    // `/info/refs?service=git-receive-pack` and `/git-receive-pack` for
    // push. Both endpoints proxy to the matching git subprocess.
    if method == "GET" && url.starts_with("/info/refs") {
        let ct = if push {
            "application/x-git-receive-pack-advertisement"
        } else {
            "application/x-git-upload-pack-advertisement"
        };
        head(&mut stream, ct)?;
        let mut announced = Vec::new();
        write_pkt(&mut announced, format!("# service={service}\n").as_bytes());
        announced.extend_from_slice(b"0000");
        chunk(&mut stream, &announced)?;
        cmd.args([subcommand, "--http-backend-info-refs"]).arg(repo);
        relay(cmd, &[], &mut stream, service)
    } else if method == "POST" && (url == "/git-upload-pack" || url == "/git-receive-pack") {
        let ct = if push {
            "application/x-git-receive-pack-result"
        } else {
            "application/x-git-upload-pack-result"
        };
        head(&mut stream, ct)?;
        cmd.args([subcommand, "--stateless-rpc"]).arg(repo);
        relay(cmd, &body, &mut stream, service)
    } else {
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
    }
}

fn head(stream: &mut TcpStream, ct: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

/// `bytes` as a chunk of the body.
fn chunk(stream: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(format!("{:x}\r\n", bytes.len()).as_bytes())?;
    stream.write_all(bytes)?;
    stream.write_all(b"\r\n")?;
    stream.flush()
}

/// Run `cmd` with `input` on its stdin and pass its stdout on to `stream`
/// as it is written, and the last chunk behind it where it ended well.
fn relay(
    mut cmd: Command,
    input: &[u8],
    stream: &mut TcpStream,
    service: &str,
) -> std::io::Result<()> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    // read beside the relay: a full stderr pipe would stop the service
    let mut stderr = child.stderr.take().unwrap();
    let said = thread::spawn(move || {
        let mut said = Vec::new();
        let _ = stderr.read_to_end(&mut said);
        said
    });
    // a service that ends before it reads all of it is told of below
    let _ = child.stdin.take().unwrap().write_all(input);
    let mut out = child.stdout.take().unwrap();
    let mut piece = vec![0u8; 64 * 1024];
    let relayed = loop {
        match out.read(&mut piece) {
            Ok(0) => break Ok(()),
            Ok(read) => {
                if let Err(gone) = chunk(stream, &piece[..read]) {
                    break Err(gone);
                }
            }
            Err(error) => break Err(error),
        }
    };
    // the service ends once a client that went away has its output closed
    drop(out);
    let status = child.wait()?;
    let said = said.join().unwrap();
    relayed?;
    if !status.success() {
        eprintln!("{service} failed: {}", String::from_utf8_lossy(&said));
        return Ok(());
    }
    stream.write_all(b"0\r\n\r\n")?;
    stream.flush()
}

fn write_pkt(out: &mut Vec<u8>, payload: &[u8]) {
    let total = payload.len() + 4;
    out.extend_from_slice(format!("{total:04x}").as_bytes());
    out.extend_from_slice(payload);
}
