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

/// Another transaction changes the row of a request and has not ended, as the
/// publication of it does. Who asks what became of the request is told what was
/// committed of it, and does not wait to be told that nobody knows.
#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn what_became_of_a_request_is_told_while_the_request_is_being_changed() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let touched = held
        .execute(
            "UPDATE alt_requests SET state=state WHERE repository='repository'",
            &[],
        )
        .unwrap();
    assert_eq!(touched, 1);

    let mut reader = f.reopen();
    let (credential, key) = (f.credential.clone(), first.publication.key.clone());
    let (tell, told) = std::sync::mpsc::channel();
    let asking = std::thread::spawn(move || {
        let recorded = reader.query(&credential, &key);
        let _ = tell.send(matches!(recorded, Ok(RequestObservation::Recorded { .. })));
        recorded.map(drop).map_err(|error| format!("{error:?}"))
    });
    let answer = told.recv_timeout(std::time::Duration::from_secs(20));
    held.commit().unwrap();
    let ended = asking.join().unwrap();
    assert_eq!((answer, ended), (Ok(true), Ok(())));
}

#[test]
#[ignore = "a measurement of the reads of a repository, for --nocapture; requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_time_of_the_reads_of_a_repository() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let key = first.publication.key.clone();
    for round in 0..5 {
        let began = std::time::Instant::now();
        for _ in 0..500 {
            f.store
                .repository_info(&f.credential, "repository")
                .unwrap();
            f.store.query(&f.credential, &key).unwrap();
        }
        println!("round {round}: 1000 reads in {:?}", began.elapsed());
    }
}
