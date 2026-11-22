//! The operations a request recorded, as the command reports them and as
//! a later query of the request does.

use std::path::Path;

use alt_json::Value;

use super::machine_results::{fixture, ok, run};

/// The ids of the operations that carry `request` in the log, oldest first.
fn logged(root: &Path, request: &str) -> Vec<Value> {
    let log = ok(root, &["op-log", "--json", "-n", "1000"]);
    let log: Value = alt_json::from_slice(&log.stdout).unwrap();
    let ops = log["ops"].as_array().unwrap().iter().rev();
    ops.filter(|op| op["request_id"] == request)
        .map(|op| op["id"].clone())
        .collect()
}

/// A command reports the operations it recorded, whether its request id
/// was given or drawn for the run, and whichever way it ran; so does a
/// later query of the request. Other requests' operations lie before and
/// between them in the log.
#[test]
fn a_request_reports_the_operations_recorded_under_it_and_no_others() {
    let root = fixture();
    for (round, daemon) in [(0, false), (1, true), (2, false)] {
        let given = format!("{round:032x}");
        for id in [Some(given.as_str()), None] {
            std::fs::write(root.path().join("a.txt"), format!("{round} {id:?}\n")).unwrap();
            let mut args = vec!["add", "a.txt", "--json"];
            if let Some(id) = id {
                args.splice(0..0, ["--request-id", id]);
            }
            let add = run(root.path(), &args, daemon);
            let said = String::from_utf8_lossy(&add.stderr).into_owned();
            assert!(add.status.success(), "{said}");
            let value: Value = alt_json::from_slice(&add.stdout).unwrap();
            let request = value["request_id"].as_str().unwrap().to_owned();
            assert!(id.is_none_or(|id| id == request));
            let recorded = logged(root.path(), &request);
            // the intent of the index change and its completion
            assert_eq!(recorded.len(), 2, "{request}");
            assert_eq!(value["operation_ids"].as_array().unwrap(), &recorded);
            let status = ok(root.path(), &["request", "status", &request, "--json"]);
            let status: Value = alt_json::from_slice(&status.stdout).unwrap();
            assert_eq!(
                status["request"]["operation_ids"].as_array().unwrap(),
                &recorded
            );
            assert_eq!(
                status["request"]["recorded_operation_ids"]
                    .as_array()
                    .unwrap(),
                &recorded
            );
        }
    }
}
