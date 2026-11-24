//! A signed push that comes in by an account of the users file, over HTTP:
//! the key that signed it and the account are two names, and the push is
//! held to what the policy lets each of them do.

use std::fs;

use crate::altd_server::spawn_server_with_env;
use crate::altd_server_certified::{pushed, told};
use crate::altd_server_untrusted::repository;

#[test]
fn a_signed_push_by_an_account_is_held_to_the_policy_of_the_key_and_of_the_account() {
    let tmp = alt_os::temp::tempdir().unwrap();
    let dir = tmp.path();
    let root = dir.join("root");
    let docs = root.join("docs");
    repository(&docs);
    let (sec, public) = alt_sign::SecretKey::generate();
    fs::create_dir_all(docs.join(".alt/trust")).unwrap();
    fs::write(docs.join(".alt/trust/dave.pub"), public.to_text()).unwrap();
    let policy =
        "human:bob -> branch_deny=refs/heads/kept\nhuman:dave -> branch_deny=refs/heads/his\n";
    fs::write(docs.join(".alt/policy"), policy).unwrap();
    let line = |user: &str, token: &str| {
        let hash = alt_hash::blake3::hash(token.as_bytes()).to_hex();
        format!("{user}\t{hash}\t*:rw\n")
    };
    let users = [line("bob", "his"), line("erin", "hers")].concat();
    fs::write(root.join("users"), users).unwrap();
    let server = spawn_server_with_env(&[("ALT_SERVER_ROOT", root.to_str().unwrap())]);
    let here = format!("http://{}/docs", server.addr);
    let by = |account: &str, token: &str| format!("http://{account}:{token}@{}/docs/", server.addr);
    let (bob, erin) = (by("bob", "his"), by("erin", "hers"));

    // a push to `branch` that dave signed, come in by the account of `url`
    let signed = |url: &str, branch: &str| {
        let (nonce, main) = told(url, dir);
        let updates = [alt_wire::RefUpdate {
            name: format!("refs/heads/{branch}"),
            old: None,
            new: Some(alt_git_codec::ObjectId::from_hex(main.as_bytes()).unwrap()),
        }];
        let sha1 = alt_git_codec::HashAlgo::Sha1;
        let payload = alt_wire::push_certificate("dave", &here, &nonce, &updates, sha1);
        let sig = sec.sign(&payload).to_text().trim().to_owned();
        let caps = format!("alt-principal=dave alt-sig={sig} alt-nonce={nonce} alt-pushee={here}");
        pushed(url, dir, branch, &main, &caps)
    };
    let taken = |report: &str, branch: &str| report.contains(&format!("ok refs/heads/{branch}"));
    // the account alone, unsigned, is held to its own policy
    let (_, main) = told(&bob, dir);
    let report = pushed(&bob, dir, "kept", &main, "");
    assert!(!taken(&report, "kept"), "{report}");
    let report = pushed(&bob, dir, "plain", &main, "");
    assert!(taken(&report, "plain"), "{report}");
    // what the account may not, the key that may does not take off it
    let report = signed(&bob, "kept");
    assert!(!taken(&report, "kept"), "{report}");
    assert!(report.contains("ng refs/heads/kept"), "{report}");
    // nor does the account take off what the key may not
    let report = signed(&erin, "his");
    assert!(!taken(&report, "his"), "{report}");
    // by an account that may, the same push of the same key is taken
    let report = signed(&erin, "kept");
    assert!(taken(&report, "kept"), "{report}");
    // and what both may, by the account that is held back elsewhere
    let report = signed(&bob, "other");
    assert!(taken(&report, "other"), "{report}");
}
