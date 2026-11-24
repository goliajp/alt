//! Signed pushes written by hand: a signature is taken where it is over a
//! certificate of the nonce the repository gave for one push and of the
//! repository itself, and nowhere else.

use std::fs;
use std::path::Path;

use crate::altd_server::spawn_server;
use crate::altd_server_untrusted::repository;

fn pkt(line: &str) -> Vec<u8> {
    format!("{:04x}{line}", line.len() + 4).into_bytes()
}

fn curl(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("curl")
        .current_dir(dir)
        .args(["-sS", "--max-time", "60"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What the repository at `url` tells before a push: the nonce it gives, and
/// the commit `main` is at.
fn told(url: &str, dir: &Path) -> (String, String) {
    let advert = curl(dir, &[&format!("{url}info/refs?service=git-receive-pack")]);
    assert!(advert.contains(" alt-pushee"), "{advert}");
    let after = |mark: &str| advert.split_once(mark).unwrap().1.to_owned();
    let nonce: String = (after("alt-nonce=").chars())
        .take_while(char::is_ascii_hexdigit)
        .collect();
    let main = advert.split_once(" refs/heads/main").unwrap().0;
    (nonce, main[main.len() - 40..].to_owned())
}

/// Pushes the creation of `branch` at `at` with `caps` and no objects; the report.
fn pushed(url: &str, dir: &Path, branch: &str, at: &str, caps: &str) -> String {
    let zero = "0".repeat(40);
    let line = format!("{zero} {at} refs/heads/{branch}\0report-status {caps}\n");
    let mut pack = b"PACK\0\0\0\x02\0\0\0\0".to_vec();
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    fs::write(
        dir.join("request"),
        [pkt(&line), b"0000".to_vec(), pack].concat(),
    )
    .unwrap();
    let service = format!("{url}git-receive-pack");
    curl(dir, &["--data-binary", "@request", &service])
}

#[test]
fn a_signed_push_is_taken_over_the_nonce_and_the_repository_it_is_for_and_once() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    repository(&origin);
    let (sec, public) = alt_sign::SecretKey::generate();
    fs::create_dir_all(origin.join(".alt/trust")).unwrap();
    fs::write(origin.join(".alt/trust/alice.pub"), public.to_text()).unwrap();
    fs::write(
        origin.join(".alt/policy"),
        "human:anonymous -> require-signed\n",
    )
    .unwrap();
    let server = spawn_server(&origin);
    let url = format!("http://{}/", server.addr);
    let here = format!("http://{}", server.addr);
    let dir = tmp.path();

    let update = |branch: &str, at: &str| alt_wire::RefUpdate {
        name: format!("refs/heads/{branch}"),
        old: None,
        new: Some(alt_git_codec::ObjectId::from_hex(at.as_bytes()).unwrap()),
    };
    let signed = |payload: &[u8]| sec.sign(payload).to_text().trim().to_owned();
    let sha1 = alt_git_codec::HashAlgo::Sha1;
    let certified = |branch: &str, at: &str, nonce: &str, pushee: &str| {
        let updates = [update(branch, at)];
        let payload = alt_wire::push_certificate("alice", pushee, nonce, &updates, sha1);
        let sig = signed(&payload);
        format!("alt-principal=alice alt-sig={sig} alt-nonce={nonce} alt-pushee={pushee}")
    };

    // over the nonce and this repository: taken
    let (nonce, main) = told(&url, dir);
    assert_eq!(nonce.len(), 32);
    let caps = certified("one", &main, &nonce, &here);
    let report = pushed(&url, dir, "one", &main, &caps);
    assert!(report.contains("ok refs/heads/one"), "{report}");
    // the same signature for the same update of another branch name is none,
    // and the nonce was good once
    let report = pushed(&url, dir, "two", &main, &caps);
    assert!(report.contains("ng refs/heads/two"), "{report}");
    assert!(report.contains("replay or expired"), "{report}");

    // over the nonce and the updates alone, as before certificates: not taken
    let (nonce, _) = told(&url, dir);
    let updates = [update("three", &main)];
    let payload = alt_wire::canonical_push_payload_with_nonce(&updates, Some(&nonce), sha1);
    let caps = format!(
        "alt-principal=alice alt-sig={} alt-nonce={nonce}",
        signed(&payload)
    );
    let report = pushed(&url, dir, "three", &main, &caps);
    assert!(report.contains("ng refs/heads/three"), "{report}");
    assert!(
        report.contains("no nonce of this server and no repository"),
        "{report}"
    );
    // over the updates alone: good anywhere, any number of times, and not taken
    let payload = alt_wire::canonical_push_payload(&updates, sha1);
    let caps = format!("alt-principal=alice alt-sig={}", signed(&payload));
    let report = pushed(&url, dir, "three", &main, &caps);
    assert!(report.contains("ng refs/heads/three"), "{report}");

    // a certificate for another repository, with a nonce of this one: what a
    // server that was pushed to could send on
    let (nonce, _) = told(&url, dir);
    for elsewhere in [format!("{here}/other"), "http://other.example".to_owned()] {
        let caps = certified("four", &main, &nonce, &elsewhere);
        let report = pushed(&url, dir, "four", &main, &caps);
        assert!(report.contains("ng refs/heads/four"), "{report}");
        assert!(report.contains("another repository"), "{report}");
    }
    // the nonce was not spent on those: the certificate for this repository is taken
    let caps = certified("four", &main, &nonce, &here);
    let report = pushed(&url, dir, "four", &main, &caps);
    assert!(report.contains("ok refs/heads/four"), "{report}");
}
