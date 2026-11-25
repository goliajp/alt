//! The bounds of the HTTP listener and its workers, as the configuration has them.

pub(super) mod memory;
mod proxies;

#[derive(Clone, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct HttpBounds {
    pub connections: usize,
    pub workers: usize,
    pub header_bytes: usize,
    pub headers: usize,
    pub header_timeout_ms: u64,
    pub body_timeout_ms: u64,
    pub work_timeout_ms: u64,
    pub connection_timeout_ms: u64,
    pub database_connect_timeout_ms: u64,
    pub database_statement_timeout_ms: u64,
    pub upload_ttl_ms: u64,
    pub worker_lease_ms: u64,
    pub approval_ttl_ms: u64,
    /// The addresses or networks (`10.0.0.0/8`) of the reverse proxies in front
    /// of the listener. Only what these append to `X-Forwarded-For` is taken for
    /// the address of a caller; with none named the header is not read.
    #[json(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_proxies: Vec<String>,
    /// How many connections one address may have in use at once; a quarter of
    /// `connections` where it is not given.
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub connections_each: Option<usize>,
    /// How many request bodies one caller may have being received at once; an
    /// eighth of `connections` where it is not given.
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub receiving_each: Option<usize>,
    /// How many megabytes of request bodies may be held that no worker has taken
    /// yet; four of the largest body there may be where it is not given.
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub receiving_megabytes: Option<u64>,
    /// How many megabytes the packs of pushes may take in memory while they are
    /// verified, all at once; 4096, or what one pack of the largest object there
    /// may be takes where that is more, where it is not given. The bases of
    /// deltas that memory has no room for are written to files, under
    /// `temporary` in the directory of the stored bytes, and may take as much.
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub indexing_megabytes: Option<u64>,
    /// How many megabytes the responses to fetches that are written to files may
    /// take on disk until they are sent, all at once, of which one address has a
    /// quarter; half of what is free on the disk of the stored bytes when the
    /// server starts where it is not given.
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub sending_megabytes: Option<u64>,
    /// How many KiB a second a response larger than the quarter of its address
    /// is taken at, at the least, over each 30 seconds while another waits for
    /// its turn to be large; it is let go of where it is taken slower. 1024
    /// where it is not given.
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub large_sending_kib: Option<u64>,
}

impl HttpBounds {
    /// the bytes a second a large response another waits for is taken at
    pub(super) fn large_pace(&self) -> u64 {
        let told = self.large_sending_kib.map(|kib| kib.saturating_mul(1024));
        told.unwrap_or(alt_shares::LARGE_RATE)
    }
}

#[cfg(test)]
mod tests {
    use crate::operator::Config;

    #[test]
    fn a_large_response_another_waits_for_is_taken_at_a_mebibyte_a_second_or_as_told() {
        let text = include_str!("../../example-config.json");
        let mut config: Config = alt_json::from_str(text).unwrap();
        assert_eq!(config.http.large_sending_kib, None);
        assert_eq!(config.http.large_pace(), 1 << 20);
        config.http.large_sending_kib = Some(256);
        assert_eq!(config.http.large_pace(), 256 << 10);
        let written = alt_json::to_vec(&config).unwrap();
        let read: Config = alt_json::from_slice(&written).unwrap();
        assert_eq!(read.http.large_sending_kib, Some(256));
    }
}
