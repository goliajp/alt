use alt_remote_protocol::{RequestKey, Scope};
use alt_wire_http::native::{Error, Options, Transport};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};

fn options() -> Options {
    Options {
        timeout: Duration::from_secs(2),
        response_bytes: 1024,
        chunk_bytes: 1024,
        content_bytes: 4096,
        allow_loopback_http: true,
    }
}
fn token() -> String {
    format!("alt1_{}", "a".repeat(64))
}
fn key() -> RequestKey {
    RequestKey {
        scope: Scope {
            installation_id: "installation".into(),
            repository_instance_id: "repository".into(),
            recovery_epoch: 1,
        },
        actor_id: "actor".into(),
        request_id: "request".into(),
    }
}
fn response(bytes: Vec<u8>, operation: impl FnOnce(&Transport)) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            assert!(headers.len() < 8192);
            assert_eq!(stream.read(&mut byte).unwrap(), 1);
            headers.push(byte[0]);
        }
        stream.write_all(&bytes).unwrap();
    });
    let transport = Transport::new(&format!("http://{address}"), token(), options()).unwrap();
    operation(&transport);
    thread.join().unwrap();
}
#[test]
fn native_connection_rejects_cleartext_remote_credentials_and_nonorigin_urls() {
    for url in [
        "http://example.test",
        "http://localhost:1234",
        "https://user:secret@example.test",
        "https://example.test/path",
        "https://example.test/?secret=x",
        "https://example.test/#fragment",
        "file:///tmp/socket",
    ] {
        assert!(Transport::new(url, token(), options()).is_err(), "{url}");
    }
    let mut strict = options();
    strict.allow_loopback_http = false;
    assert!(Transport::new("http://127.0.0.1:1234", token(), strict).is_err());
    assert!(Transport::new("https://example.test", token(), strict).is_ok());
    assert!(
        Transport::new(
            "https://example.test",
            "alt1_bad\r\nAuthorization: x".into(),
            strict
        )
        .is_err()
    );
}
#[test]
fn native_does_not_follow_redirects_or_report_missing_write_response_as_failure() {
    response(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), |client| {
        assert!(matches!(client.query(&key()), Err(Error::Rejected {status:302,..})));
    });
    response(vec![], |client| {
        assert!(
            matches!(client.publish(&key()), Err(Error::OutcomeUnknown {key: actual}) if *actual == key())
        );
    });
    response(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{\"partial\":".to_vec(), |client| {
        assert!(matches!(client.publish(&key()), Err(Error::OutcomeUnknown {..})));
    });
}
#[test]
fn native_bounds_json_and_checks_response_scope() {
    response(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2048\r\nConnection: close\r\n\r\n".to_vec(), |client| {
        assert!(matches!(client.query(&key()), Err(Error::Limit(_))));
    });
    let mut other = key();
    other.actor_id = "other".into();
    let body = serde_json::to_vec(&alt_remote_protocol::RequestObservation::NotSeen { key: other })
        .unwrap();
    let mut bytes = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).into_bytes();
    bytes.extend(body);
    response(bytes, |client| {
        assert!(matches!(
            client.query(&key()),
            Err(Error::Invalid("request response scope"))
        ));
    });
}
#[test]
fn native_download_rejects_corrupt_bytes_despite_matching_headers() {
    let hash = alt_hash::blake3::hash(b"right").to_hex().to_string();
    let bytes = format!("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nAlt-Content-Blake3: {hash}\r\nConnection: close\r\n\r\nwrong").into_bytes();
    response(bytes, |client| {
        let mut staged = Vec::new();
        assert!(matches!(
            client.download(&key().scope, &hash, 5, &mut staged),
            Err(Error::Invalid("download content hash"))
        ));
    });
}
