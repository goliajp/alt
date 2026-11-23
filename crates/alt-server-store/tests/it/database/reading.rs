//! Reads of a repository while a push holds its row.
use super::*;

/// Another transaction holds the row of the repository, as a push does while its
/// objects are checked. The reads of the repository are answered meanwhile, with
/// what was committed before.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_read_of_a_repository_waits_for_no_push_to_it() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    held.query_one(row, &[]).unwrap();

    let mut reader = f.reopen();
    let (credential, key) = (f.credential.clone(), first.publication.key.clone());
    let (tell, told) = std::sync::mpsc::channel();
    let reading = std::thread::spawn(move || {
        let info = reader.repository_info(&credential, "repository").unwrap();
        tell.send(info.refs.len()).unwrap();
        let recorded = reader.query(&credential, &key).unwrap();
        tell.send(matches!(recorded, RequestObservation::Recorded { .. }) as usize)
            .unwrap();
        let read = reader.read_snapshot(&credential, "repository", 60_000, 1000);
        tell.send(read.unwrap().refs.len()).unwrap();
    });
    let wait = std::time::Duration::from_secs(20);
    let answers: Vec<_> = (0..3).map(|_| told.recv_timeout(wait)).collect();
    drop(held);
    reading.join().unwrap();
    assert_eq!(answers, [Ok(1), Ok(1), Ok(1)]);
}
