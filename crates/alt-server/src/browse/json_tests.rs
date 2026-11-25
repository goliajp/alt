use alt_json_testkit::fixtures::{assert_stored, written_line};
use alt_json_testkit::rng::Rng;
use alt_json_testkit::support::Arbitrary;

use super::*;

fn any<T: Arbitrary>(rng: &mut Rng) -> T {
    T::arbitrary(rng)
}

#[test]
fn responses_are_written_as_recorded() {
    let mut rng = Rng::new(0xd700);
    let rng = &mut rng;
    let mut lines = String::new();
    for _ in 0..40 {
        lines += &written_line(&TreePage {
            commit: any(rng),
            path: any(rng),
            parent: any(rng),
            display_path: any(rng),
            entries: (0..rng.below(3))
                .map(|_| TreeEntry {
                    name: any(rng),
                    path: any(rng),
                    display_path: any(rng),
                    oid: any(rng),
                    kind: rng.pick(&["tree", "blob", "commit"]),
                    bytes: any(rng),
                })
                .collect(),
            next: any(rng),
        });
        lines += &written_line(&BlobPreview {
            oid: any(rng),
            bytes: any(rng),
            text: any(rng),
            truncated: any(rng),
            download_url: any(rng),
        });
        lines += &written_line(&CommitSummary {
            oid: any(rng),
            parents: any(rng),
            author: any(rng),
            timestamp: any(rng),
            message: any(rng),
        });
    }
    assert_stored(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json/browse-responses.txt"
        ),
        &lines,
    );
}
