//! Whether a configuration is one the server may run with, and the bounds its
//! listener takes from it.
use super::*;

/// under the directory of the stored bytes: what the serving process writes for a time
const TEMPORARY: &str = "temporary";

impl Config {
    pub fn validate(&self) -> Result<()> {
        use alt_remote_protocol::DeploymentMode;
        let mut identifiers = std::collections::BTreeSet::new();
        if self.change_links.len() > 32 {
            return Err(Error::Config("change-link configuration limit"));
        }
        for channel in &self.change_links {
            channel.validate(&self.instance)?;
            if !identifiers.insert((
                &channel.scope.repository_instance_id,
                channel.scope.recovery_epoch,
                &channel.channel,
            )) {
                return Err(Error::Config("duplicate change-link configuration"));
            }
        }
        match (&self.cloud, self.instance.deployment_mode) {
            (Some(cloud), DeploymentMode::Cloud) => {
                cloud
                    .audience
                    .validate()
                    .map_err(|_| Error::Config("Cloud audience"))?;
                if cloud
                    .provision_digest
                    .as_deref()
                    .is_some_and(|d| !golia_platform_contract::lifecycle::lower_hex_32(d))
                {
                    return Err(Error::Config("Cloud provision digest"));
                }
                let a = &cloud.audience;
                if a.product != golia_platform_contract::Product::Alt
                    || a.installation_id.as_str() != self.instance.installation_id
                    || a.workspace_id.as_str() != self.instance.workspace_id
                    || a.generation.0 != self.instance.runtime_generation
                    || !cloud.runtime_credential_file.is_absolute()
                    || self.browser.as_ref().is_some_and(|b| {
                        b.public_origin != a.origin.as_str() || b.session_ttl_ms > 8 * 3_600_000
                    })
                {
                    return Err(Error::Config("Cloud instance binding"));
                }
            }
            (None, DeploymentMode::SelfHosted) => (),
            _ => {
                return Err(Error::Config(
                    "deployment mode requires exact Cloud configuration",
                ));
            }
        }
        if let Some(browser) = &self.browser {
            alt_browser_auth::PublicOrigin::parse(&browser.public_origin)
                .map_err(|_| Error::Config("browser public origin"))?;
            if !browser.assets_directory.is_absolute()
                || !(60_000..=43_200_000).contains(&browser.session_ttl_ms)
            {
                return Err(Error::Config("browser assets/session limits"));
            }
        }
        self.limits
            .validate()
            .map_err(|_| Error::Config("transfer limits"))?;
        alt_remote_protocol::Scope {
            installation_id: self.instance.installation_id.clone(),
            repository_instance_id: self.instance.workspace_id.clone(),
            recovery_epoch: self.instance.runtime_generation,
        }
        .validate()
        .map_err(|_| Error::Config("instance identity"))?;
        if self.version != 1
            || !self.listen.ip().is_loopback()
            || !self.bytes_directory.is_absolute()
            || !self.receipt_key_file.is_absolute()
            || self.database_environment.is_empty()
            || self.database_environment.len() > 128
            || !self
                .database_environment
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(Error::Config(
                "version, private listener, paths or explicit database environment",
            ));
        }
        let h = &self.http;
        if h.connections == 0
            || h.connections > 65536
            || h.workers == 0
            || h.workers > h.connections
            || h.header_bytes < 8192
            || h.header_bytes > 1024 * 1024
            || h.headers == 0
            || h.headers > 1024
            || [
                h.header_timeout_ms,
                h.body_timeout_ms,
                h.work_timeout_ms,
                h.connection_timeout_ms,
                h.database_connect_timeout_ms,
                h.database_statement_timeout_ms,
                h.upload_ttl_ms,
                h.worker_lease_ms,
                h.approval_ttl_ms,
            ]
            .into_iter()
            .any(|n| n == 0 || n > 86_400_000)
            || h.database_statement_timeout_ms > 300_000
            || h.database_connect_timeout_ms > 300_000
            || h.worker_lease_ms < h.work_timeout_ms
            || h.connections_each == Some(0)
            || h.receiving_each == Some(0)
            || h.receiving_megabytes == Some(0)
            || h.indexing_megabytes.is_some_and(|megabytes| {
                megabytes.saturating_mul(1 << 20) < crate::git::least_verifying_bytes(self.limits)
            })
            || h.trusted_proxies.len() > 64
            || (h.trusted_proxies.iter()).any(|proxy| proxy.parse::<crate::Network>().is_err())
        {
            return Err(Error::Config("HTTP/worker bounds"));
        }
        if let Some(portable) = self.portable {
            portable
                .verification(self.limits, self.tasks)
                .map_err(|_| Error::Config("portable validation bounds"))?;
        }
        let t = &self.tasks;
        if [
            t.objects,
            t.records,
            t.edges,
            t.structured_bytes,
            t.paths,
            t.path_bytes,
            t.walk_steps,
        ]
        .into_iter()
        .any(|n| n == 0 || n > u32::MAX as usize)
            || t.artifact_bytes == 0
            || t.total_bytes < t.artifact_bytes
            || t.work_bytes < t.total_bytes
        {
            return Err(Error::Config("task validation bounds"));
        }
        Ok(())
    }
    /// [`Self::http_options`] for the process that serves, called once before it
    /// has a second thread. What the process writes for a time (request bodies,
    /// responses to fetches, the bases of deltas) goes to `temporary` under the
    /// directory of the stored bytes from here on, on the disk the bytes are on
    /// and not among the temporary files of the host, which may be kept in
    /// memory. The directory is its owner's alone, and what the process before
    /// this one left in it is removed.
    pub fn serving_options(&self) -> Result<HttpOptions> {
        if self.instance.installation_id == TEMPORARY {
            return Err(Error::Config("installation id"));
        }
        let directory = self.bytes_directory.join(TEMPORARY);
        alt_os::temp::emptied(&directory).map_err(|_| Error::File)?;
        // SAFETY: the caller is the one thread there is: nothing else reads or
        // writes the environment
        unsafe { std::env::set_var("TMPDIR", &directory) };
        let mut options = self.http_options();
        // as much in files as in memory
        let bases = directory.join("bases");
        let files = alt_pack_segment::BaseFiles::open(&bases, options.verifying.all());
        options.base_files = Some(Box::leak(Box::new(files.map_err(|_| Error::File)?)));
        Ok(options)
    }
    pub fn http_options(&self) -> HttpOptions {
        let h = &self.http;
        HttpOptions {
            connections: h.connections,
            workers: h.workers,
            header_bytes: h.header_bytes,
            headers: h.headers,
            header_timeout: Duration::from_millis(h.header_timeout_ms),
            body_timeout: Duration::from_millis(h.body_timeout_ms),
            work_timeout: Duration::from_millis(h.work_timeout_ms),
            connection_timeout: Duration::from_millis(h.connection_timeout_ms),
            trusted_proxies: (h.trusted_proxies.iter())
                .filter_map(|proxy| proxy.parse().ok())
                .collect(),
            expiry_interval: Duration::from_secs(30),
            password_places: 2,
            connections_each: h.connections_each.unwrap_or((h.connections / 4).max(1)),
            receiving_each: h.receiving_each.unwrap_or((h.connections / 8).max(1)),
            receiving_bytes: match h.receiving_megabytes {
                Some(megabytes) => megabytes.saturating_mul(1 << 20),
                None => {
                    let body =
                        (self.limits.total_bytes).saturating_add(self.limits.request_bytes as u64);
                    body.saturating_mul(4)
                }
            },
            // one for as long as the process serves
            verifying: Box::leak(Box::new(alt_pack_segment::Memory::new(
                match h.indexing_megabytes {
                    Some(megabytes) => megabytes.saturating_mul(1 << 20),
                    None => crate::git::least_verifying_bytes(self.limits).max(4 << 30),
                },
            ))),
            base_files: None,
            native: NativeOptions {
                portable: self.portable,
                task_limits: self.tasks,
                upload_ttl_ms: h.upload_ttl_ms,
                worker_lease_ms: h.worker_lease_ms,
                approval_ttl_ms: h.approval_ttl_ms,
            },
        }
    }
}
