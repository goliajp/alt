//! A read whose request stops waiting before the worker is done.
use super::*;

#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn read_that_outlives_its_request_is_retryable() {
    let f = Fixture::new();
    let (factory, mut options) = Running::parts(&f, None, vec![]);
    options.work_timeout = Duration::from_secs(2);
    let server = Running::serve(factory, options, None, false);
    // the content lookup waits for this table for as long as the test holds it
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    held.batch_execute("LOCK TABLE alt_blobs IN ACCESS EXCLUSIVE MODE")
        .unwrap();
    let (status, failure) = server.command(
        "GET",
        &format!(
            "/v1/repositories/repository/epochs/1/content/{}",
            "a".repeat(64)
        ),
        &f.token,
    );
    assert_eq!(status, 503);
    assert_eq!(failure["code"], "temporarily_unavailable");
    assert_eq!(failure["retry"], "same_request");
    held.commit().unwrap();
}
