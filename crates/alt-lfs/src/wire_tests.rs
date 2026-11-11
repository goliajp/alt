//! The batch API messages, held to recorded bytes and verdicts: `=` lines
//! are requests as they must be written, `A` and `R` lines are responses
//! that must be accepted and rejected.

use alt_json::Value;

use super::{BatchObject, BatchRequest, BatchResponse};

const FIXTURE: &str = include_str!("fixtures/batch.txt");

#[test]
fn requests_are_written_as_recorded() {
    let mut requests = 0;
    for line in FIXTURE.lines().filter_map(|line| line.strip_prefix("= ")) {
        let recorded: Value = alt_json::from_str(line).unwrap();
        let objects = recorded["objects"].as_array().unwrap();
        let request = BatchRequest {
            operation: "download",
            transfers: ["basic"],
            objects: objects
                .iter()
                .map(|object| BatchObject {
                    oid: object["oid"].as_str().unwrap(),
                    size: object["size"].as_u64().unwrap(),
                })
                .collect(),
        };
        assert_eq!(alt_json::to_string(&request).unwrap(), line);
        requests += 1;
    }
    assert_eq!(requests, 44);
}

#[test]
fn responses_are_judged_as_recorded() {
    let (mut accepted, mut rejected) = (0, 0);
    for line in FIXTURE.lines() {
        if let Some(text) = line.strip_prefix("A ") {
            assert!(alt_json::from_str::<BatchResponse>(text).is_ok(), "{text}");
            accepted += 1;
        } else if let Some(text) = line.strip_prefix("R ") {
            assert!(alt_json::from_str::<BatchResponse>(text).is_err(), "{text}");
            rejected += 1;
        }
    }
    assert_eq!((accepted, rejected), (257, 154));
}

#[test]
fn response_fields() {
    let response: BatchResponse = alt_json::from_str(
        r#"{"transfer":"basic","objects":[
            {"oid":"a","size":1,"actions":{"download":{"href":"h","header":{"K":"v"},"expires_in":5}}},
            {"oid":"b","error":{"code":404,"message":"gone"}}]}"#,
    )
    .unwrap();
    let download = response.objects[0]
        .actions
        .as_ref()
        .unwrap()
        .download
        .as_ref()
        .unwrap();
    assert_eq!(
        (download.href.as_str(), download.header["K"].as_str()),
        ("h", "v")
    );
    assert!(response.objects[0].error.is_none() && response.objects[1].actions.is_none());
    assert_eq!(response.objects[1].error.as_ref().unwrap().message, "gone");
}
