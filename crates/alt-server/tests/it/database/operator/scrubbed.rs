//! The scrub an operator asks for, of a repository or of all of them.
use super::*;
use alt_json::{Value, json};

/// Three repositories, one of them with a segment in a file. All are read. A
/// name that is none of them is an error and not a repository found sound.
/// With the file of the segment gone its repository cannot be read: the other
/// two are told of all the same, the one beside them with why, and the command
/// fails.
#[test]
#[ignore = "requires isolated PostgreSQL and a real operator subprocess; exercised by closing gate"]
fn a_scrub_tells_of_the_repositories_it_read_beside_one_it_could_not_and_fails() {
    let db = OperatorDb::new();
    let (mut store, admin, _) = maintenance::prepare(&db);
    let token = alt_server::operator::read_token(std::path::Path::new(&admin)).unwrap();
    maintenance::push_large(&mut store, &token);
    for name in ["aside", "sound"] {
        let quota = ["--quota-bytes", "100000000"];
        db.ok(&[
            &["--credential", &admin, "repository-create", name],
            &quota[..],
        ]
        .concat());
    }
    let all = db.ok(&["--credential", &admin, "scrub"]);
    assert_eq!(
        (&all["repositories"], &all["segments"], &all["objects"]),
        (&json!(3), &json!(1), &json!(3))
    );
    assert_eq!(
        (&all["unread_objects"], &all["failed"]),
        (&json!(0), &Value::Null)
    );

    let told = |args: &[&str]| -> (Option<i32>, Value) {
        let out = db.run(&[&["--credential", &admin, "scrub"], args].concat());
        let told = alt_json::from_slice(&out.stdout);
        (out.status.code(), told.unwrap_or(Value::Null))
    };
    let (code, typo) = told(&["repositry"]);
    assert_eq!(code, Some(1), "{typo}");
    assert_eq!(typo["repositories"], 0);
    let no_repository = format!("precondition failed: {}", alt_server_store::NO_REPOSITORY);
    let failed = json!([{"repository":"repositry","error":no_repository}]);
    assert_eq!(typo["failed"], failed);

    let files = maintenance::segment_files(&Config::read(&db.config).unwrap().bytes_directory);
    assert_eq!(files.len(), 1);
    std::fs::remove_file(&files[0]).unwrap();
    let (code, partly) = told(&[]);
    assert_eq!(code, Some(1), "{partly}");
    assert_eq!(
        (&partly["repositories"], &partly["unread_objects"]),
        (&json!(2), &json!(0))
    );
    let failed = partly["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{partly}");
    assert_eq!(failed[0]["repository"], "repository");
    assert_eq!(told(&["sound"]).0, Some(0));
}
