//! What the index of the moments requests ended at costs the database for each
//! push, measured in the instructions its processes take.
use super::*;
use std::io::Read;
use std::process::Stdio;

/// the processes of the database that serve this instance
fn backends(f: &Fixture) -> Vec<String> {
    let all = "SELECT pid::text FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND backend_type='client backend'";
    let rows = f.client().query(all, &[]).unwrap();
    rows.iter().map(|row| row.get(0)).collect()
}

/// The instructions the processes `pids` take in user space while `work` runs, as
/// `perf` counts them; nothing where there is no `perf` to ask.
fn instructions(pids: &[String], work: impl FnOnce()) -> Option<u64> {
    let counting = Command::new("perf")
        .args(["stat", "-x", ",", "-e", "instructions:u", "-p"])
        .arg(pids.join(","))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    // attached before the work begins
    std::thread::sleep(Duration::from_millis(500));
    work();
    let mut counting = counting.ok()?;
    let mut interrupt = Command::new("kill");
    interrupt.args(["-INT", &counting.id().to_string()]);
    let interrupted = interrupt.status();
    let mut counted = String::new();
    let mut stderr = counting.stderr.take().expect("piped");
    let read = stderr.read_to_string(&mut counted);
    // waited for before anything that went wrong is told
    counting.wait().unwrap();
    assert!(interrupted.unwrap().success());
    read.unwrap();
    let line = counted.lines().find(|line| line.contains("instructions"))?;
    line.split(',').next()?.parse().ok()
}

/// Pushes `main` to `url` as [`ok`] runs Git, with what went over HTTP
/// written down: a push that fails is told with the status and the body of
/// each answer, which say which bound of the service it met.
fn pushed(work: &Path, token: &str, url: &str) {
    let trace = work.join("trace");
    let auth = alt_base64::STANDARD.encode(format!("alt:{token}"));
    let output = Command::new("git")
        .args(["push", "-q", url, "main"])
        .current_dir(work)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {auth}"))
        .env("GIT_TRACE_CURL", &trace)
        .output()
        .unwrap();
    if !output.status.success() {
        let traced = std::fs::read_to_string(&trace).unwrap_or_default();
        let answered: Vec<&str> = (traced.lines())
            .filter(|line| line.contains("<= Recv header: HTTP/") || line.contains("<= Recv data"))
            .collect();
        panic!(
            "git push: {}\nanswers:\n{}",
            String::from_utf8_lossy(&output.stderr),
            answered.join("\n")
        );
    }
    std::fs::remove_file(&trace).unwrap();
}

#[test]
#[ignore = "a measurement, for --nocapture and with ALT_MEASURE_PUSHES: counts instructions with perf; requires isolated PostgreSQL, loopback HTTP and real Git"]
fn the_instructions_the_database_takes_for_a_push_with_and_without_the_index_of_moments() {
    let pushes = std::env::var("ALT_MEASURE_PUSHES").ok();
    let pushes: usize = pushes.and_then(|told| told.parse().ok()).unwrap_or(3);
    for indexed in [true, false, true, false] {
        let f = Fixture::new();
        let server = Running::start(&f);
        let temp = alt_os::temp::tempdir().unwrap();
        let work = temp.path();
        ok(work, &f.token, &["init", "-q", "--initial-branch=main"]);
        identity(work, &f.token);
        let url = format!("{}/git/repository", server.url);
        let push = |n: usize| {
            std::fs::write(work.join("file"), n.to_string()).unwrap();
            ok(work, &f.token, &["add", "file"]);
            ok(work, &f.token, &["commit", "-qm", "more"]);
            pushed(work, &f.token, &url);
        };
        if !indexed {
            let drop = "DROP INDEX alt_requests_settled";
            f.client().batch_execute(drop).unwrap();
        }
        // the connections the service keeps are open, and their plans made
        (0..10).for_each(push);
        let pids = backends(&f);
        let counted = instructions(&pids, || (10..10 + pushes).for_each(push));
        let each = counted.map(|all| all / pushes as u64);
        println!(
            "index of moments {indexed}: {pushes} pushes, {} processes, {counted:?} instructions, {each:?} a push",
            pids.len()
        );
        let settled = "SELECT count(*) FROM alt_requests WHERE settled_at IS NOT NULL";
        let settled: i64 = f.client().query_one(settled, &[]).unwrap().get(0);
        assert_eq!(settled, 10 + pushes as i64);
    }
}
