//! What the server of these tests is given: its limits and how it listens.
use alt_remote_protocol::Limits;
use alt_server::{HttpOptions, NativeOptions};
use std::time::Duration;

pub(super) fn limits() -> Limits {
    Limits {
        request_bytes: 8 * 1024 * 1024,
        ref_updates: 100,
        record_roots: 100,
        manifest_entries: 10000,
        graph_objects: 10000,
        graph_edges: 100000,
        structured_bytes: 4 * 1024 * 1024,
        tree_paths: 10000,
        path_bytes: 4096,
        walk_steps: 1_000_000,
        chunk_bytes: 1024 * 1024,
        object_bytes: 256 * 1024 * 1024,
        total_bytes: 512 * 1024 * 1024,
    }
}
pub(super) fn task_limits() -> alt_task_service::TaskLimits {
    alt_task_service::TaskLimits {
        objects: 10000,
        records: 1000,
        edges: 10000,
        structured_bytes: 4 * 1024 * 1024,
        total_bytes: 512 * 1024 * 1024,
        work_bytes: 4 * 1024 * 1024 * 1024,
        artifact_bytes: 256 * 1024 * 1024,
        paths: 10000,
        path_bytes: 4096,
        walk_steps: 1_000_000,
    }
}

pub(super) fn options() -> HttpOptions {
    HttpOptions {
        connections: 16,
        workers: 4,
        header_bytes: 16384,
        headers: 32,
        header_timeout: Duration::from_secs(3),
        body_timeout: Duration::from_secs(15),
        work_timeout: Duration::from_secs(30),
        connection_timeout: Duration::from_secs(60),
        trusted_proxies: vec![],
        connections_each: 16,
        receiving_each: 4,
        receiving_bytes: 1 << 30,
        expiry_interval: Duration::from_secs(30),
        password_places: 2,
        verifying: Box::leak(Box::new(alt_server::PackMemory::new(4 << 30))),
        base_files: None,
        sending: alt_server::shares::Shares::passed_by_one_alone(u64::MAX),
        native: NativeOptions {
            portable: None,
            task_limits: task_limits(),
            upload_ttl_ms: 600_000,
            worker_lease_ms: 60_000,
            approval_ttl_ms: 300_000,
        },
    }
}
