//! The shim answers before the service has its answer ready.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn pkt(out: &mut Vec<u8>, line: &str) {
    out.extend_from_slice(format!("{:04x}{line}", line.len() + 4).as_bytes());
}

/// pack-objects is held until the client has read the status line, so a
/// shim that sends its status line only once the service has written its
/// whole answer never sends one
#[test]
fn the_status_line_comes_before_the_pack_is_made() {
    let dir = alt_os::temp::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a"), "a\n").unwrap();
    git(&repo, &["add", "a"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "-qm",
            "a",
        ],
    );
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let gate = dir.path().join("gate");
    let hook = dir.path().join("hook");
    let script = format!(
        "#!/bin/sh\nwhile [ ! -e '{}' ]; do sleep 0.05; done\nexec \"$@\"\n",
        gate.display()
    );
    std::fs::write(&hook, script).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    // git takes this hook only from configuration of the command line
    let env = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "uploadpack.packObjectsHook"),
        ("GIT_CONFIG_VALUE_0", hook.to_str().unwrap()),
    ];
    let env = env.map(|(k, v)| (k.to_owned(), v.to_owned())).to_vec();
    let url = super::spawn_with_env(repo, env);

    let mut body = Vec::new();
    pkt(&mut body, "command=fetch\n");
    body.extend_from_slice(b"0001");
    pkt(&mut body, &format!("want {}\n", head.trim()));
    pkt(&mut body, "done\n");
    body.extend_from_slice(b"0000");
    let mut stream = TcpStream::connect(url.strip_prefix("http://").unwrap()).unwrap();
    let request = format!(
        "POST /git-upload-pack HTTP/1.0\r\nGit-Protocol: version=2\r\n\
         Content-Type: application/x-git-upload-pack-request\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(&body).unwrap();
    // only so that a shim which never answers ends the test
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    let read = reader.read_line(&mut status);
    std::fs::write(&gate, "").unwrap();
    assert!(
        read.is_ok(),
        "no status line while pack-objects was held: {read:?}"
    );
    assert_eq!(status, "HTTP/1.0 200 OK\r\n");

    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    let ended = rest.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let (head, answer) = rest.split_at(ended);
    let head = String::from_utf8_lossy(head);
    assert!(
        head.contains("application/x-git-upload-pack-result"),
        "{head}"
    );
    assert!(answer.windows(13).any(|w| w == b"000dpackfile\n"));
    assert!(answer.windows(4).any(|w| w == b"PACK"));
    assert!(answer.ends_with(b"0000"));
}
