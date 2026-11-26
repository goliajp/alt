//! What the refs a push was read with name now on the way to their commits is
//! not lost by a ref that was deleted meanwhile: an annotated tag another ref
//! still names may be pushed again by its id.

use super::led::packed;
use super::read_moved::{HOOKED, held_while};
use super::read_moved_lost::{body, pushed};
use super::*;

/// Two tags name one annotated tag. A third is pushed to it, with a pack of
/// nothing, and one of the two is deleted while the push waits to be
/// published: the other still names it, and the push is published.
#[test]
#[ignore = "requires isolated PostgreSQL, loopback HTTP and real Git; exercised by server acceptance and closing gate"]
fn a_push_of_an_annotated_tag_another_ref_still_names_is_published_though_one_that_did_is_deleted()
{
    let _hooked = HOOKED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let server = Running::start(&f);
    let p = pushed(&f, &server);
    let git = |args: &[&str]| text(ok(&p.work, &f.token, args));
    git(&["tag", "-a", "-m", "v1", "v1", &p.main]);
    let tag = git(&["rev-parse", "v1"]);
    git(&[
        "push",
        "-q",
        &p.url,
        "refs/tags/v1",
        "refs/tags/v1:refs/tags/v2",
    ]);
    let listed = git(&["ls-remote", &p.url, "refs/tags/v2"]);
    assert!(listed.starts_with(&tag), "{listed}");
    let zero = "0".repeat(40);
    let body = body(
        &[(&zero, &tag, "refs/tags/v3")],
        &packed(&p.work, &f.token, &[]),
    );
    let (status, report) = held_while(&f, &p.url, &f.token, body, || {
        git(&["push", "-q", &p.url, ":refs/tags/v1"]);
    });
    assert_eq!(status, 200, "{report}");
    assert!(report.contains("ok refs/tags/v3"), "{report}");
    let listed = git(&["ls-remote", &p.url, "refs/tags/v3"]);
    assert!(listed.starts_with(&tag), "{listed}");
}
