use alt_remote_protocol::login::*;
use alt_wire_http::device::{Client, Error};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};
fn serve(reply: impl FnOnce(&str) -> String + Send + 'static, call: impl FnOnce(&Client)) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let remote_origin = origin.clone();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            assert!(headers.len() < 8192);
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).unwrap();
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        assert!(headers.contains("application/x-www-form-urlencoded"));
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|n| n.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        assert!(
            String::from_utf8(body)
                .unwrap()
                .contains("client_id=alt-cli")
        );
        stream.write_all(reply(&remote_origin).as_bytes()).unwrap();
    });
    call(&Client::new(&origin, true).unwrap());
    worker.join().unwrap();
}
fn json(status: u16, value: alt_json::Value) -> String {
    let body = value.to_string();
    format!(
        "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn authorization(origin: &str) -> alt_json::Value {
    alt_json::json!({"device_code":"a".repeat(64),"user_code":"ABCD-2345","verification_uri":format!("{origin}/#device"),"verification_uri_complete":format!("{origin}/#device?user_code=ABCD-2345"),"expires_in":300,"interval":5})
}
#[test]
fn device_transport_binds_confirmation_origin_and_refuses_redirects_and_unbounded_bodies() {
    for origin in [
        "http://example.test",
        "http://localhost:4000",
        "https://x.test/path",
        "https://user:secret@x.test",
        "https://x.test/?token=x",
    ] {
        assert!(Client::new(origin, true).is_err());
    }
    assert!(Client::new("http://127.0.0.1:4000", false).is_err());
    serve(
        |origin| json(200, authorization(origin)),
        |client| assert_eq!(client.begin().unwrap().user_code, "ABCD-2345"),
    );
    serve(
        |origin| {
            let mut reply = authorization(origin);
            reply["verification_uri_complete"] = alt_json::json!("https://other.example/#device");
            json(200, reply)
        },
        |client| assert!(matches!(client.begin(), Err(Error::Invalid))),
    );
    serve(
        |_| {
            "HTTP/1.1 302 Found\r\nLocation: https://other.example\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
        },
        |client| assert!(matches!(client.begin(), Err(Error::Invalid))),
    );
    serve(
        |_| json(200, alt_json::json!({"padding":"x".repeat(20_000)})),
        |client| assert!(matches!(client.begin(), Err(Error::Invalid))),
    );
}
#[test]
fn device_poll_retains_standard_pending_and_slow_down_semantics() {
    for error in [
        DeviceError::AuthorizationPending,
        DeviceError::SlowDown,
        DeviceError::AccessDenied,
        DeviceError::ExpiredToken,
    ] {
        serve(
            move |_| json(400, alt_json::to_value(DeviceFailure { error }).unwrap()),
            |client| {
                assert!(
                    matches!(client.poll(&"a".repeat(64)),Err(Error::Rejected(actual)) if actual==error)
                )
            },
        );
    }
    serve(
        |_| {
            json(
                200,
                alt_json::json!({"access_token":format!("alt1_{}","b".repeat(64)),"token_type":"Bearer","scope":"account","expires_in":3600}),
            )
        },
        |client| assert_eq!(client.poll(&"a".repeat(64)).unwrap().expires_in, 3600),
    );
}
