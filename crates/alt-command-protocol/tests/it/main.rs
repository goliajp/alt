use alt_command_protocol::*;
#[test]
fn exact_legacy_bytes_and_malformed_lengths() {
    let response = Response {
        exit_code: 7,
        stdout: b"ok".to_vec(),
        stderr: b"err".to_vec(),
    };
    let bytes = vec![7, 2, 0, 0, 0, b'o', b'k', 3, 0, 0, 0, b'e', b'r', b'r'];
    assert_eq!(response.encode(), bytes);
    assert_eq!(Response::decode(&bytes).unwrap(), response);
    for end in 0..bytes.len() {
        assert!(Response::decode(&bytes[..end]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(Response::decode(&extra).is_err());
    assert!(Response::decode(&[0, 255, 255, 255, 255]).is_err());
}
#[test]
fn completion_binds_source_request_and_exact_response() {
    let intent = Intent {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: "1".repeat(32),
        binding_digest: "2".repeat(64),
        repository: "3".repeat(32),
        workspace: "4".repeat(32),
        started_ms: 1,
    };
    let bytes = Response {
        exit_code: 0,
        stdout: b"{}".to_vec(),
        stderr: vec![],
    }
    .encode();
    let mut completion = Completion {
        version: 1,
        hash_algorithm: "blake3".into(),
        request_id: intent.request_id.clone(),
        binding_digest: intent.binding_digest.clone(),
        finished_ms: 2,
        response_digest: alt_hash::blake3::hash(&bytes).to_hex().to_string(),
        response_bytes: bytes.len() as u64,
        operations: vec![],
        outcome: "applied".into(),
    };
    assert!(validate_completion(&intent, &completion, &bytes).is_ok());
    completion.request_id = "5".repeat(32);
    assert!(validate_completion(&intent, &completion, &bytes).is_err());
    completion.request_id = intent.request_id.clone();
    completion.response_digest = "6".repeat(64);
    assert!(validate_completion(&intent, &completion, &bytes).is_err());
}
