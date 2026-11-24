//! Two server processes and one database.
use super::*;

fn copied(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copied(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
    let mode = std::fs::metadata(from).unwrap().permissions();
    std::fs::set_permissions(to, mode).unwrap();
}

/// A server on another machine has a data directory of its own, which the lock
/// of the first one's directory says nothing of, and the same database. It is
/// turned away by the lock of the database, and the first serves on. One that
/// is started on the directory of the first is turned away by that directory.
#[test]
#[ignore = "requires isolated PostgreSQL and real server subprocesses; exercised by closing gate"]
fn a_second_server_of_a_database_is_turned_away_whatever_directory_it_has() {
    let db = OperatorDb::new();
    db.ok(&["secret", "receipt-key", "--output", &db.path("receipt.key")]);
    assert_eq!(db.ok(&["initialize"])["initialized"], true);
    let first = ServerProcess(
        db.command()
            .arg("serve")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut first = first;
    let mut line = String::new();
    let mut said = BufReader::new(first.0.stdout.take().unwrap());
    said.read_line(&mut line).unwrap();
    let started: alt_json::Value = alt_json::from_str(&line).unwrap();
    let url = format!("http://{}/healthz", started["address"].as_str().unwrap());
    let healthy = || {
        let asked = ureq::get(&url).timeout(Duration::from_secs(15));
        asked.call().unwrap().status()
    };
    assert_eq!(healthy(), 200);

    // the same configuration with a copy of the data in another place
    let other = db.root.path().join("bytes-of-another-machine");
    copied(&db.root.path().join("bytes"), &other);
    let mut config: alt_json::Value =
        alt_json::from_slice(&std::fs::read(&db.config).unwrap()).unwrap();
    config["bytes_directory"] = other.to_str().unwrap().into();
    let elsewhere = db.root.path().join("server-elsewhere.json");
    std::fs::write(&elsewhere, alt_json::to_vec(&config).unwrap()).unwrap();
    let mut second = Command::new(env!("CARGO_BIN_EXE_alt-server"));
    second.arg("--config").arg(&elsewhere).arg("serve");
    second.env("ALT_OPERATOR_DATABASE", &db.dsn);
    second.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut second = ServerProcess(second.spawn().unwrap());
    // one that serves would not end: it is given as long as the lock is waited for, and more
    let until = std::time::Instant::now() + Duration::from_secs(60);
    let ended = loop {
        if let Some(status) = second.0.try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < until, "the second serves");
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut why = String::new();
    let mut told = second.0.stderr.take().unwrap();
    std::io::Read::read_to_string(&mut told, &mut why).unwrap();
    assert!(!ended.success(), "{why}");
    assert!(why.contains("another server serves this database"), "{why}");
    assert_eq!(healthy(), 200);

    let same = db.run(&["serve"]);
    let why = String::from_utf8_lossy(&same.stderr);
    assert!(!same.status.success(), "{why}");
    assert!(why.contains("another server serves these bytes"), "{why}");
    assert_eq!(healthy(), 200);
}
