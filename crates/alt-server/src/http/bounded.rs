use super::HttpOptions;
use std::time::Duration;

impl HttpOptions {
    /// whether each bound is one a server can run with
    pub(super) fn bounded(&self) -> bool {
        let times = [
            self.header_timeout,
            self.body_timeout,
            self.work_timeout,
            self.connection_timeout,
            self.expiry_interval,
        ];
        self.connections != 0
            && self.connections_each != 0
            && self.receiving_each != 0
            && self.workers != 0
            && self.header_bytes >= 8192
            && self.headers != 0
            && !times.contains(&Duration::ZERO)
    }
}
