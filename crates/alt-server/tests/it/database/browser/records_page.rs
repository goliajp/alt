//! A page of records holds what it may read of them, and the next page the rest.
use super::*;
use crate::database::change_links::task_payload_with_source;
use crate::database::native_led::{client, sent};

/// the tasks published, each with a record of some eight hundred kilobytes
const TASKS: usize = 22;

/// More records than a page may read the payloads of at once, which come to
/// some seventeen megabytes together: the first page ends where the next
/// record would take it past what it may read, and tells where the next page
/// begins; the next page has the rest. Each record is listed once.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by browser acceptance and closing gate"]
fn records_whose_payloads_are_more_than_a_page_reads_are_listed_over_two_pages() {
    let mut f = Fixture::new();
    user(&mut f, "reader", false);
    f.store
        .set_grants(&f.credential, "repository", "reader", true, false, &[])
        .unwrap();
    let server = start(&mut f);
    let native = client(&server, &f.token);
    let long = "x".repeat(800_000);
    for n in 0..TASKS {
        let purpose = format!("task {n} {long}");
        let request = format!("large-task-{n}");
        let payload = task_payload_with_source((&request, &purpose), None);
        assert!(sent(&native, &payload), "{n}");
    }
    let (cookie, _) = login(&server, "reader");
    let page = |after: &str| {
        let path = format!("/web/repositories/repository/records/task?after={after}");
        let (status, page) = get(&server, &path, &cookie);
        assert_eq!(status, 200, "{page}");
        let items = page["items"].as_array().unwrap();
        let ids: Vec<String> = (items.iter())
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect();
        (ids, page["next"].as_str().map(str::to_owned))
    };
    let (first, next) = page("");
    assert!(first.len() < TASKS, "{}", first.len());
    assert_eq!(next.as_ref(), first.last());
    let (second, last) = page(&next.unwrap());
    assert_eq!(last, None);
    let mut all = [first, second].concat();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), TASKS);
}
