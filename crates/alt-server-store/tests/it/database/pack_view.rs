//! The references a view holds for each way of asking for them.
use super::*;
use alt_server_store::{ARRAY_TIPS, RefFilter, RefScope};

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_view_by_tips_holds_the_references_at_the_wanted_objects_and_tells_of_the_others() {
    let mut f = Fixture::new();
    let oid = |digit: char| digit.to_string().repeat(40);
    let rows = [
        ("refs/heads/a", oid('1')),
        ("refs/heads/b", oid('2')),
        ("refs/heads/same-as-a", oid('1')),
        ("refs/tags/v1", oid('3')),
        ("refs/tags/v2", oid('2')),
    ];
    let mut client = f.client();
    for (name, oid) in &rows {
        client
            .execute(
                "INSERT INTO alt_refs(repository,name,oid) VALUES('repository',$1,$2)",
                &[&name.to_string(), oid],
            )
            .unwrap();
    }
    // the same repository name elsewhere never shows through
    client
        .batch_execute(&format!(
            "INSERT INTO alt_repositories(id,recovery_epoch,object_format,quota_bytes) VALUES('other',1,'sha1',0);
             INSERT INTO alt_refs(repository,name,oid) VALUES('other','refs/heads/elsewhere','{}')",
            oid('9')
        ))
        .unwrap();
    let absent = oid('f');
    let cases: [(&[String], bool); 9] = [
        (&[], false),
        (&[], true),
        (&[oid('1')], false),
        (&[oid('1'), oid('1'), oid('1')], false),
        (std::slice::from_ref(&absent), false),
        (&[absent.clone(), oid('2')], true),
        (&[oid('9')], false),
        (&[oid('1'), oid('2'), oid('3')], false),
        (
            &[oid('3'), absent.clone(), oid('1'), oid('2'), oid('2')],
            true,
        ),
    ];
    // ids no reference has, to reach the lengths either side of where the statement changes
    let padding: Vec<String> = (0..=ARRAY_TIPS)
        .map(|index| format!("e{index:039x}"))
        .collect();
    for (wants, tags) in cases {
        for length in [wants.len(), ARRAY_TIPS, ARRAY_TIPS + 1] {
            let mut padded = wants.to_vec();
            padded.extend_from_slice(&padding[..length - wants.len()]);
            let view = f
                .store
                .pack_view_with(
                    &f.credential,
                    "repository",
                    RefFilter::Tips {
                        wants: &padded,
                        tags,
                    },
                )
                .unwrap();
            let expected: Vec<(&str, String)> = rows
                .iter()
                .filter(|(name, oid)| {
                    wants.contains(oid) || (tags && name.starts_with("refs/tags/"))
                })
                .map(|(name, oid)| (*name, oid.clone()))
                .collect();
            let held: Vec<(&str, String)> = view
                .refs
                .iter()
                .map(|(name, oid)| (name, oid.to_string()))
                .collect();
            assert_eq!(held, expected, "wants {wants:?} of {length} tags {tags}");
            assert_eq!(
                view.ref_scope,
                RefScope::Filtered {
                    other_tips: rows.iter().any(|(_, oid)| !wants.contains(oid)),
                },
                "wants {wants:?} of {length} tags {tags}"
            );
        }
    }
}
