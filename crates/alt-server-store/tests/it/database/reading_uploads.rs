//! The reads about an upload, an export, an import and a read in progress while a
//! push holds the row of their repository.
use super::*;

/// What each of the reads answers, told as it is answered. The answers are the
/// same whoever holds the row of the repository: those of a request that is
/// committed and selects nothing, of an export and an import nobody began, and of
/// a read that may go on.
fn reads(
    mut reader: Store,
    credential: Credential,
    (key, commit): (RequestKey, ObjectId),
    tell: std::sync::mpsc::Sender<String>,
) {
    let read = reader.read_snapshot(&credential, "repository", 60_000, 1000);
    let read = read.unwrap();
    let digest = "0".repeat(64);
    let told = |name: &str, answer: String| tell.send(format!("{name}: {answer}")).unwrap();
    let status = reader.upload_status(&credential, &key);
    told("upload status", format!("{:?}", status.map(|_| ())));
    let chunk = reader.selected_chunk(&credential, &key, &digest);
    told("selected chunk", format!("{chunk:?}"));
    let upload = reader.selected_upload(&credential, &key, &digest);
    told(
        "selected upload",
        format!("{:?}", upload.map(|all| all.len())),
    );
    let package = super::portable::portable_limits().package;
    let export = reader.query_portable_export(&credential, &key.scope, "absent", package);
    told("export", format!("{:?}", export.map(|_| ())));
    let object = reader.open_snapshot_object(&credential, &read, commit);
    told(
        "read in progress",
        format!("{:?}", object.map(|found| found.1)),
    );
    let import = reader.query_portable_import(&credential, &key);
    told("import", format!("{:?}", import.map(|_| ())));
    let upload = reader.selected_portable_upload(&credential, &key, &digest);
    told("upload of an import", format!("{:?}", upload.map(|_| ())));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn the_reads_about_an_upload_an_export_an_import_and_a_read_in_progress_wait_for_no_push() {
    let mut f = Fixture::new();
    let first = Payload::new("first", None);
    first.publish_new(&mut f.store, &f.credential);
    let asked = (first.publication.key.clone(), first.commit);
    let wait = std::time::Duration::from_secs(20);
    let answers = |f: &Fixture| {
        let (reader, credential) = (f.reopen(), f.credential.clone());
        let (tell, told) = std::sync::mpsc::channel();
        let asked = asked.clone();
        let reading = std::thread::spawn(move || reads(reader, credential, asked, tell));
        let answers: Vec<_> = (0..7).map(|_| told.recv_timeout(wait).ok()).collect();
        (answers, reading)
    };
    let (free, reading) = answers(&f);
    reading.join().unwrap();
    assert!(free.iter().all(Option::is_some), "{free:?}");

    let mut holder = f.client();
    let mut held = holder.transaction().unwrap();
    let row = "SELECT 1 FROM alt_repositories WHERE id='repository' FOR UPDATE";
    held.query_one(row, &[]).unwrap();
    let (while_held, reading) = answers(&f);
    held.commit().unwrap();
    reading.join().unwrap();
    assert_eq!(while_held, free);
}
