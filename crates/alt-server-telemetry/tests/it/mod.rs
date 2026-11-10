use alt_server_telemetry::{
    Actor, Id, Method, Metrics, Outcome, Phase, PhaseGuard, Route, Trace, Transport,
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
fn capture() -> Capture {
    static LOG: OnceLock<Capture> = OnceLock::new();
    LOG.get_or_init(|| {
        let capture = Capture::default();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .json()
                .with_writer(capture.clone())
                .finish(),
        )
        .unwrap();
        capture
    })
    .clone()
}
fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}
fn trace(metrics: Arc<Metrics>) -> Trace {
    capture();
    Trace::start(metrics, id("instance"), Route::Request, Method::Post)
}

#[test]
fn lost_response_and_later_work_outcome_are_separate_correlated_facts() {
    let capture = capture();
    let metrics = Metrics::new(None).unwrap();
    let t = trace(metrics.clone());
    t.identity(Some(id("repo")), Some(id("request-one")), None);
    t.actor(Actor::Agent);
    t.headers(503);
    let worker = t.clone();
    t.transport(503, 19, Transport::Dropped);
    std::thread::spawn(move || {
        worker.in_scope(|| {
            let phase = PhaseGuard::current(Phase::Commit);
            worker.identity(None, None, Some(id("42")));
            phase.finish(Outcome::Ok);
        })
    })
    .join()
    .unwrap();
    // Repeated cleanup cannot produce two terminal observations or negative inflight.
    t.transport(200, 100, Transport::Complete);
    let lines: Vec<serde_json::Value> = String::from_utf8(capture.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|v| v["fields"]["trace_id"] == t.id())
        .collect();
    let terminal: Vec<_> = lines
        .iter()
        .filter(|v| v["fields"]["event"] == "response.ended")
        .collect();
    assert_eq!(terminal.len(), 1, "captured: {lines:?}");
    assert_eq!(terminal[0]["fields"]["outcome"], "dropped");
    let work = lines
        .iter()
        .find(|v| v["fields"]["event"] == "work.ended")
        .unwrap();
    assert_eq!(work["fields"]["outcome"], "ok");
    assert_eq!(work["fields"]["transaction_id"], "42");
    assert_eq!(
        work["fields"]["trace_id"],
        terminal[0]["fields"]["trace_id"]
    );
    let text = String::from_utf8(metrics.encode().unwrap()).unwrap();
    assert!(text.contains("alt_http_inflight 0"));
    assert!(!text.contains("request-one") && !text.contains("repo=\"") && !text.contains(t.id()));
    assert!(text.contains("alt_operation_outcomes_total{outcome=\"ok\",phase=\"commit\"} 1"));
}

#[test]
fn nested_and_panicking_workers_restore_context_and_report_interruption() {
    let metrics = Metrics::new(None).unwrap();
    let outer = trace(metrics.clone());
    let inner = trace(metrics.clone());
    assert!(Trace::current().is_none());
    outer.in_scope(|| {
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.in_scope(|| {
                let _work = PhaseGuard::current(Phase::Validate);
                panic!("controlled test panic");
            })
        }));
        assert!(failed.is_err());
        assert_eq!(Trace::current().unwrap().id(), outer.id());
    });
    assert!(Trace::current().is_none());
    outer.transport(0, 0, Transport::Dropped);
    inner.transport(0, 0, Transport::Dropped);
    let text = String::from_utf8(metrics.encode().unwrap()).unwrap();
    assert!(
        text.contains("alt_operation_outcomes_total{outcome=\"interrupted\",phase=\"validate\"} 1")
    );
    assert!(!text.contains("outcome=\"ok\""));
}

#[test]
fn hostile_methods_have_one_label_and_identifiers_cannot_inject_log_lines() {
    capture();
    let metrics = Metrics::new(None).unwrap();
    for n in 0..1000 {
        let t = Trace::start(
            metrics.clone(),
            id("instance"),
            Route::Unknown,
            Method::from_http(&format!("attacker-{n}")),
        );
        t.transport(404, 0, Transport::Complete);
    }
    let text = String::from_utf8(metrics.encode().unwrap()).unwrap();
    assert!(!text.contains("attacker"));
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("alt_http_responses_total{"))
            .count(),
        1
    );
    assert!(text.contains("transport=\"complete\"} 1000"));
    for invalid in [
        "",
        "bad\nline",
        "https://secret.invalid",
        "password with space",
        &"a".repeat(129),
    ] {
        assert!(Id::new(invalid).is_none());
    }
}

#[test]
fn independent_instances_have_independent_metrics_and_trace_namespaces() {
    let a = Metrics::new(None).unwrap();
    let b = Metrics::new(None).unwrap();
    let first = trace(a.clone());
    let second = trace(b.clone());
    assert_ne!(first.id(), second.id());
    first.transport(200, 7, Transport::Complete);
    second.transport(403, 0, Transport::Failed);
    let at = String::from_utf8(a.encode().unwrap()).unwrap();
    let bt = String::from_utf8(b.encode().unwrap()).unwrap();
    assert!(at.contains("status_class=\"2xx\""));
    assert!(!at.contains("status_class=\"4xx\""));
    assert!(bt.contains("status_class=\"4xx\""));
    assert!(!bt.contains("status_class=\"2xx\""));
}
mod body;

#[test]
fn process_logging_drops_when_full_and_excludes_other_targets_even_with_rust_log() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "checks::bounded_writer_child"])
        .env("ALT_TELEMETRY_WRITER_CHILD", "1")
        .env("RUST_LOG", "trace")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("bounded log writer blocked its caller or failed to drain");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "executed by the parent test in a separate process to install the actual global logger"]
fn bounded_writer_child() {
    use std::sync::Condvar;
    use std::time::Duration;
    if std::env::var("ALT_TELEMETRY_WRITER_CHILD").as_deref() != Ok("1") {
        return;
    }
    #[derive(Default)]
    struct State {
        entered: bool,
        open: bool,
        bytes: Vec<u8>,
    }
    type Shared = Arc<(Mutex<State>, Condvar)>;
    struct BlockWriter(Shared);
    impl Write for BlockWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let (state, notify) = &*self.0;
            let mut state = state.lock().unwrap();
            state.entered = true;
            notify.notify_all();
            while !state.open {
                state = notify.wait(state).unwrap();
            }
            state.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    struct Release(Shared);
    impl Drop for Release {
        fn drop(&mut self) {
            let (state, notify) = &*self.0;
            state.lock().unwrap().open = true;
            notify.notify_all();
        }
    }
    let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
    let logging = alt_server_telemetry::install_logging(BlockWriter(shared.clone()), 2).unwrap();
    // The release guard is declared after logging, so panic also unblocks flush.
    let release = Release(shared.clone());
    let metrics = Metrics::new(Some(logging.dropped())).unwrap();
    tracing::info!(target: "postgres", password="secret-sentinel-do-not-log");
    let first = Trace::start(metrics.clone(), id("instance"), Route::Health, Method::Get);
    {
        let state = shared.0.lock().unwrap();
        let (state, timeout) = shared
            .1
            .wait_timeout_while(state, Duration::from_secs(5), |s| !s.entered)
            .unwrap();
        assert!(
            state.entered && !timeout.timed_out(),
            "actual writer did not receive a log"
        );
    }
    for _ in 0..100 {
        let t = Trace::start(
            metrics.clone(),
            id("instance"),
            Route::Request,
            Method::Post,
        );
        t.transport(200, 0, Transport::Complete);
    }
    first.transport(200, 0, Transport::Complete);
    let dropped = logging.dropped().dropped_lines();
    assert!(
        dropped > 0,
        "full writer must report loss instead of blocking producers"
    );
    let text = String::from_utf8(metrics.encode().unwrap()).unwrap();
    assert!(text.contains(&format!("alt_log_dropped_total {dropped}")));
    drop(release);
    drop(logging);
    let bytes = shared.0.lock().unwrap().bytes.clone();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("alt_server_telemetry"));
    assert!(!text.contains("secret-sentinel-do-not-log"));
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}

#[test]
fn missing_or_future_backup_never_reports_a_fresh_successful_backup_age() {
    let metrics = Metrics::new(None).unwrap();
    let mut sample = alt_server_telemetry::Snapshot {
        observed_at_unix_ms: 10_000,
        stored_bytes: 100,
        reserved_bytes: 50,
        pending_requests: 2,
        recovery_gaps: 3,
        pending_events: 4,
        pending_change_links: 5,
        recovery_change_links: 6,
        volume_total_bytes: 1_000,
        volume_available_bytes: 700,
        last_backup_created_unix_ms: Some(7_500),
    };
    let text = String::from_utf8(metrics.encode_snapshot(&sample).unwrap()).unwrap();
    assert!(text.contains("alt_backup_age_seconds 2.5"));
    assert!(text.contains("alt_change_link_events_pending 5"));
    assert!(text.contains("alt_change_link_recovery_gaps 6"));
    for value in [None, Some(10_001)] {
        sample.last_backup_created_unix_ms = value;
        let text = String::from_utf8(metrics.encode_snapshot(&sample).unwrap()).unwrap();
        assert!(text.contains("alt_backup_age_known 0"));
        assert!(
            !text
                .lines()
                .any(|line| line.starts_with("alt_backup_age_seconds "))
        );
        assert!(text.contains(if value.is_some() {
            "alt_backup_creation_known 1"
        } else {
            "alt_backup_creation_known 0"
        }));
    }
    // A prior response cannot leak its ephemeral sample into process metrics.
    assert!(
        !String::from_utf8(metrics.encode().unwrap())
            .unwrap()
            .contains("alt_backup_age_seconds")
    );
}

#[test]
fn background_attempts_keep_event_identity_without_inventing_http_responses() {
    let capture = capture();
    let metrics = Metrics::new(None).unwrap();
    let t = Trace::background(metrics.clone(), id("instance"), Route::ChangeLink);
    t.event_identity(id("event-one"));
    t.in_scope(|| {
        PhaseGuard::current(Phase::Dispatch).finish(Outcome::Unknown);
    });
    // Accidental HTTP cleanup must not create a response or decrement inflight.
    t.transport(200, 123, Transport::Complete);
    let text = String::from_utf8(metrics.encode().unwrap()).unwrap();
    assert!(text.contains("alt_http_inflight 0"));
    assert!(!text.contains("alt_http_responses_total{"));
    assert!(
        text.contains("alt_operation_outcomes_total{outcome=\"unknown\",phase=\"dispatch\"} 1")
    );
    let rows: Vec<serde_json::Value> = String::from_utf8(capture.0.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|v| v["fields"]["trace_id"] == t.id())
        .collect();
    let work = rows
        .iter()
        .find(|v| v["fields"]["event"] == "work.ended")
        .unwrap();
    assert_eq!(work["fields"]["origin"], "background");
    assert_eq!(work["fields"]["event_id"], "event-one");
    assert!(work["fields"]["request_id"].is_null());
    assert!(
        !rows
            .iter()
            .any(|v| v["fields"]["event"] == "response.ended")
    );
}
