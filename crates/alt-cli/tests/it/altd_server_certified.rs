//! Signed pushes written by hand: a signature is taken where it is over a
//! certificate of the nonce the repository gave for one push and of the
//! repository itself, and nowhere else.

use std::fs;
use std::path::Path;

use crate::altd_server::{alt, spawn_server, spawn_server_with_env};
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
    pushed_as(url, dir, (branch, at), caps, None)
}

/// [`pushed`], the request naming `host` as the server it is for where one is given.
fn pushed_as(
    url: &str,
    dir: &Path,
    (branch, at): (&str, &str),
    caps: &str,
    host: Option<&str>,
) -> String {
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
    let named = host.map(|host| format!("Host: {host}"));
    let mut args = vec!["--data-binary", "@request"];
    if let Some(named) = &named {
        args.extend(["-H", named]);
    }
    args.push(&service);
    curl(dir, &args)
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
    assert_eq!(nonce.len(), 64);
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

/// A server that is told where clients reach it takes a signed push for that
/// address and for no other: not for the name the request gives it, which is
/// its sender's word, and so not what was signed for another server and is
/// sent on under that server's name. One that is told nothing goes by the
/// name of the request, and says so when it starts. The client knows from
/// what the server tells before a push whether its signature would be taken.
#[test]
fn a_server_told_where_it_is_reached_takes_a_signed_push_for_that_address_only() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let (sec, public) = alt_sign::SecretKey::generate();
    let made = |name: &str| {
        let origin = dir.join(name);
        repository(&origin);
        fs::create_dir_all(origin.join(".alt/trust")).unwrap();
        fs::write(origin.join(".alt/trust/alice.pub"), public.to_text()).unwrap();
        let policy = "human:anonymous -> require-signed\n";
        fs::write(origin.join(".alt/policy"), policy).unwrap();
        origin
    };
    let sha1 = alt_git_codec::HashAlgo::Sha1;
    let certified = |branch: &str, at: &str, nonce: &str, pushee: &str| {
        let updates = [alt_wire::RefUpdate {
            name: format!("refs/heads/{branch}"),
            old: None,
            new: Some(alt_git_codec::ObjectId::from_hex(at.as_bytes()).unwrap()),
        }];
        let payload = alt_wire::push_certificate("alice", pushee, nonce, &updates, sha1);
        let sig = sec.sign(&payload).to_text().trim().to_owned();
        format!("alt-principal=alice alt-sig={sig} alt-nonce={nonce} alt-pushee={pushee}")
    };

    let told_of = made("told");
    let env = [
        ("ALT_SERVER_REPO", told_of.to_str().unwrap()),
        ("ALT_SERVER_ORIGINS", "https://Alt.Example"),
    ];
    let server = spawn_server_with_env(&env);
    let url = format!("http://{}/", server.addr);
    let advert = curl(dir, &[&format!("{url}info/refs?service=git-receive-pack")]);
    assert!(
        advert.contains(" alt-pushee=https://alt.example"),
        "{advert}"
    );
    let (nonce, main) = told(&url, dir);
    // for the name the request gives the server, which is not where it is reached
    let by_request = format!("http://{}", server.addr);
    let caps = certified("one", &main, &nonce, &by_request);
    let report = pushed(&url, dir, "one", &main, &caps);
    assert!(report.contains("another repository"), "{report}");
    // the request and the certificate both written for another server
    let caps = certified("one", &main, &nonce, "http://other.example");
    let report = pushed_as(&url, dir, ("one", &main), &caps, Some("other.example"));
    assert!(report.contains("another repository"), "{report}");
    // for where it is reached, whatever the request names
    let caps = certified("one", &main, &nonce, "https://alt.example");
    let report = pushed_as(&url, dir, ("one", &main), &caps, Some("other.example"));
    assert!(report.contains("ok refs/heads/one"), "{report}");

    // the client, with a key to sign with, pushing to another name of the
    // server than the server tells: it ends before a pack is made
    let clone = dir.join("clone");
    let out = alt(dir, &["clone", &url, clone.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    fs::create_dir_all(clone.join(".alt/identity")).unwrap();
    fs::write(clone.join(".alt/identity/alice.sec"), sec.to_text()).unwrap();
    let policy = "enabled = true\nprincipal = alice\n";
    fs::write(clone.join(".alt/sign-policy"), policy).unwrap();
    fs::write(clone.join("new.txt"), "new\n").unwrap();
    assert!(alt(&clone, &["add", "."]).status.success());
    assert!(alt(&clone, &["commit", "-m", "new"]).status.success());
    let out = alt(
        &clone,
        &["push", "origin", "refs/heads/main:refs/heads/main"],
    );
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{out:?}");
    assert!(
        said.contains("the server is reached at https://alt.example"),
        "{said}"
    );
    assert!(
        said.contains(&format!("this push goes to {by_request}")),
        "{said}"
    );
    drop(server);

    // told nothing: the name of the request is what a certificate is held to
    let untold = made("untold");
    let mut server = spawn_server_with_env(&[("ALT_SERVER_REPO", untold.to_str().unwrap())]);
    let url = format!("http://{}/", server.addr);
    let (nonce, main) = told(&url, dir);
    let caps = certified("one", &main, &nonce, "http://other.example");
    let report = pushed_as(&url, dir, ("one", &main), &caps, Some("other.example"));
    assert!(report.contains("ok refs/heads/one"), "{report}");
    let said = server.drain_stderr();
    assert!(said.contains("ALT_SERVER_ORIGINS is not set"), "{said}");
}
