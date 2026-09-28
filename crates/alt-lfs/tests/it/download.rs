//! The batch client against a local LFS server: contents arrive checked,
//! and a server that errs or serves the wrong bytes is reported.

use std::collections::HashMap;
use std::sync::Arc;

use alt_lfs::{Client, Error, Pointer};

/// Serves `objects` (by sha256) through a minimal batch API; `lie` swaps the
/// bytes it serves for one oid. Returns the endpoint URL.
fn serve(objects: HashMap<String, Vec<u8>>, lie: Option<String>) -> String {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let base = format!("http://{}", server.server_addr());
    let endpoint = format!("{base}/repo.git/info/lfs");
    let objects = Arc::new(objects);
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_owned();
            if url.ends_with("/objects/batch") {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).unwrap();
                let asked: serde_json::Value = serde_json::from_str(&body).unwrap();
                let answers: Vec<serde_json::Value> = asked["objects"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|o| {
                        let oid = o["oid"].as_str().unwrap();
                        if objects.contains_key(oid) {
                            serde_json::json!({"oid": oid, "size": o["size"],
                                "actions": {"download": {"href": format!("{base}/content/{oid}"),
                                    "header": {"X-Test": "yes"}}}})
                        } else {
                            serde_json::json!({"oid": oid, "size": o["size"],
                                "error": {"code": 404, "message": "Object does not exist"}})
                        }
                    })
                    .collect();
                let resp = serde_json::json!({"transfer": "basic", "objects": answers});
                req.respond(tiny_http::Response::from_string(resp.to_string()))
                    .unwrap();
            } else if let Some(oid) = url.strip_prefix("/content/") {
                let has_header = req.headers().iter().any(|h| h.field.equiv("X-Test"));
                assert!(has_header, "the action's headers must be sent");
                let mut bytes = objects[oid].clone();
                if lie.as_deref() == Some(oid) {
                    bytes.push(b'!');
                }
                req.respond(tiny_http::Response::from_data(bytes)).unwrap();
            }
        }
    });
    endpoint
}

#[test]
fn contents_arrive_and_match_their_pointers() {
    let (a, b) = (b"model weights".to_vec(), vec![7u8; 100_000]);
    let (pa, pb) = (Pointer::of(&a), Pointer::of(&b));
    let endpoint = serve(
        HashMap::from([(pa.oid.clone(), a.clone()), (pb.oid.clone(), b.clone())]),
        None,
    );
    let got = Client::new(endpoint, None)
        .download(&[pa.clone(), pb.clone()])
        .unwrap();
    assert_eq!(got, vec![(pa, a), (pb, b)]);
}

#[test]
fn wrong_bytes_are_rejected() {
    let a = b"real".to_vec();
    let pa = Pointer::of(&a);
    let endpoint = serve(HashMap::from([(pa.oid.clone(), a)]), Some(pa.oid.clone()));
    let err = Client::new(endpoint, None).download(&[pa]).unwrap_err();
    assert!(matches!(err, Error::Corrupt { .. }), "{err}");
}

#[test]
fn a_missing_object_is_reported_by_oid() {
    let pa = Pointer::of(b"never uploaded");
    let endpoint = serve(HashMap::new(), None);
    let err = Client::new(endpoint, None)
        .download(&[pa.clone()])
        .unwrap_err();
    match err {
        Error::Missing { oid, message } => {
            assert_eq!(oid, pa.oid);
            assert!(message.contains("does not exist"), "{message}");
        }
        other => panic!("{other}"),
    }
}
