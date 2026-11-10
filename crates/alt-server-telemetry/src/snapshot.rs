use prometheus::{Encoder, Gauge, TextEncoder, core::Collector};

/// One authorized database observation plus a capacity observation of its byte
/// volume. Capacity describes the filesystem (possibly shared), not a quota.
/// Backup creation is a recorded local success, not proof of offsite retention.
pub struct Snapshot {
    pub observed_at_unix_ms: u64,
    pub stored_bytes: u64,
    pub reserved_bytes: u64,
    pub pending_requests: u64,
    pub recovery_gaps: u64,
    pub pending_events: u64,
    pub pending_change_links: u64,
    pub recovery_change_links: u64,
    pub volume_total_bytes: u64,
    pub volume_available_bytes: u64,
    pub last_backup_created_unix_ms: Option<u64>,
}
impl crate::Metrics {
    /// Snapshot gauges are local to this response. Concurrent scrapes cannot
    /// combine counters from different database snapshots by overwriting gauges.
    pub fn encode_snapshot(&self, snapshot: &Snapshot) -> Result<Vec<u8>, prometheus::Error> {
        self.observe_log_losses();
        let mut families = self.registry.gather();
        let mut gauge = |name, help, value| -> Result<(), prometheus::Error> {
            let metric = Gauge::new(name, help)?;
            metric.set(value);
            families.extend(metric.collect());
            Ok(())
        };
        gauge(
            "alt_instance_observed_timestamp_seconds",
            "Database observation timestamp",
            snapshot.observed_at_unix_ms as f64 / 1000.0,
        )?;
        gauge(
            "alt_storage_logical_stored_bytes",
            "Accounted immutable content and change-link history bytes",
            snapshot.stored_bytes as f64,
        )?;
        gauge(
            "alt_storage_reserved_bytes",
            "Reserved logical upload bytes",
            snapshot.reserved_bytes as f64,
        )?;
        gauge(
            "alt_requests_pending",
            "Receiving or ready requests in current repository epochs",
            snapshot.pending_requests as f64,
        )?;
        gauge(
            "alt_requests_recovery_gaps",
            "Unfinished requests from earlier recovery epochs",
            snapshot.recovery_gaps as f64,
        )?;
        gauge(
            "alt_publication_events_pending",
            "Undelivered publication outbox records",
            snapshot.pending_events as f64,
        )?;
        gauge(
            "alt_change_link_events_pending",
            "Undelivered cross-product events in current repository epochs",
            snapshot.pending_change_links as f64,
        )?;
        gauge(
            "alt_change_link_recovery_gaps",
            "Undelivered cross-product events from earlier recovery epochs",
            snapshot.recovery_change_links as f64,
        )?;
        gauge(
            "alt_volume_total_bytes",
            "Byte-store filesystem total capacity; may be shared",
            snapshot.volume_total_bytes as f64,
        )?;
        gauge(
            "alt_volume_available_bytes",
            "Byte-store filesystem capacity available to this process; not tenant quota",
            snapshot.volume_available_bytes as f64,
        )?;
        gauge(
            "alt_backup_creation_known",
            "Whether a successful local backup creation has been recorded",
            if snapshot.last_backup_created_unix_ms.is_some() {
                1.0
            } else {
                0.0
            },
        )?;
        let age = snapshot
            .last_backup_created_unix_ms
            .and_then(|time| snapshot.observed_at_unix_ms.checked_sub(time));
        gauge(
            "alt_backup_age_known",
            "Whether recorded backup age is known and nonnegative at this observation",
            if age.is_some() { 1.0 } else { 0.0 },
        )?;
        if let Some(time) = snapshot.last_backup_created_unix_ms {
            gauge(
                "alt_backup_created_timestamp_seconds",
                "Last recorded successful local backup creation, not retention proof",
                time as f64 / 1000.0,
            )?;
        }
        if let Some(age) = age {
            gauge(
                "alt_backup_age_seconds",
                "Elapsed time since last recorded successful local backup creation",
                age as f64 / 1000.0,
            )?;
        }
        let mut bytes = Vec::new();
        TextEncoder::new().encode(&families, &mut bytes)?;
        Ok(bytes)
    }
}
