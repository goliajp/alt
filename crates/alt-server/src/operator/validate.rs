//! Whether a configuration is one the server may run with, and the bounds its
//! listener takes from it.
use super::*;

/// under the directory of the stored bytes: what the serving process writes for a time
const TEMPORARY: &str = "temporary";

/// Makes `directory` the place of this process for what it writes for a time:
/// its owner's alone and empty. The process holds a lock on it for as long as it
/// is, so that a second server of the same bytes does not empty what this one
/// writes; a link in its place is not followed.
fn temporary(directory: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(directory) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(Error::File),
        _ => {}
    }
    let mut lock = std::fs::OpenOptions::new();
    lock.read(true).custom_flags(alt_os::sys::O_NOFOLLOW);
    let lock = lock.open(directory).map_err(|_| Error::File)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Error::Config("another server serves these bytes"));
        }
        // a lock that cannot be asked for is held by nobody
        Err(std::fs::TryLockError::Error(_)) => return Err(Error::File),
    }
    std::mem::forget(lock);
    alt_os::temp::emptied(directory).map_err(|_| Error::File)
}

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
            || [h.receiving_megabytes, h.sending_megabytes].contains(&Some(0))
            || h.large_sending_kib == Some(0)
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
    /// this one left in it is removed; a process that still serves holds
    /// a lock on it, and a second one ends here.
    pub fn serving_options(&self) -> Result<HttpOptions> {
        if self.instance.installation_id == TEMPORARY {
            return Err(Error::Config("installation id"));
        }
        self.within(super::bounds::memory::of_the_container())?;
        let directory = self.bytes_directory.join(TEMPORARY);
        temporary(&directory)?;
        // SAFETY: the caller is the one thread there is: nothing else reads or
        // writes the environment
        unsafe { std::env::set_var("TMPDIR", &directory) };
        let mut options = self.http_options();
        // as much in files as in memory
        let bases = directory.join("bases");
        let files = alt_pack_segment::BaseFiles::open(&bases, options.verifying.all());
        options.base_files = Some(Box::leak(Box::new(files.map_err(|_| Error::File)?)));
        let sending = sending(self.http.sending_megabytes, self.http.workers, &directory)?;
        options.sending = sending.paced(self.http.large_pace(), alt_shares::LARGE_WINDOW);
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
                self.indexing_bytes(),
            ))),
            base_files: None,
            // told, or as much as there is until the disk it is on is known
            sending: alt_shares::Shares::passed_by_one_alone(
                (h.sending_megabytes)
                    .map_or(u64::MAX, |megabytes| megabytes.saturating_mul(1 << 20)),
                h.workers / 2,
            )
            .paced(h.large_pace(), alt_shares::LARGE_WINDOW),
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

/// The room the responses to fetches have on disk in `directory`, where they are
/// written: as many megabytes as the configuration tells, or half of what is
/// free on the disk when the server starts. Half the `workers` may send a
/// response larger than the part of one address at once.
fn sending(
    told: Option<u64>,
    workers: usize,
    directory: &Path,
) -> Result<std::sync::Arc<alt_shares::Shares>> {
    let room = match told {
        Some(megabytes) => megabytes.saturating_mul(1 << 20),
        None => alt_shares::half_of_what_is_free(directory).map_err(|_| Error::File)?,
    };
    Ok(alt_shares::Shares::passed_by_one_alone(room, workers / 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_room_for_responses_is_what_is_told_or_half_of_what_is_free_on_its_disk() {
        let work = alt_os::temp::tempdir().unwrap();
        assert_eq!(sending(Some(3), 8, work.path()).unwrap().most(), 3 << 20);
        let free = || alt_shares::half_of_what_is_free(work.path()).unwrap();
        let (before, room, after) = (free(), sending(None, 8, work.path()).unwrap(), free());
        // others write to the disk meanwhile: within a tenth of what was free
        // before and after
        let (least, most) = (before.min(after), before.max(after));
        assert!(room.most() >= least - least / 10 && room.most() <= most + most / 10);
        assert!(room.most() > 0 && room.most() < u64::MAX);
        assert!(matches!(
            sending(None, 8, &work.path().join("none")),
            Err(Error::File)
        ));
    }

    #[test]
    fn the_place_for_what_is_written_for_a_time_is_one_server_s_and_no_link() {
        let work = alt_os::temp::tempdir().unwrap();
        let directory = work.path().join("temporary");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("left"), b"by the process before").unwrap();
        temporary(&directory).unwrap();
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
        // a second server of the same bytes leaves what the first writes
        std::fs::write(directory.join("held"), b"by the one that serves").unwrap();
        let second = temporary(&directory);
        assert!(matches!(second, Err(Error::Config(_))), "{second:?}");
        assert!(directory.join("held").exists());
        // a link where the directory is to be: what it points to is left as it is
        let elsewhere = work.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("kept"), b"not of the server").unwrap();
        let link = work.path().join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        assert!(matches!(temporary(&link), Err(Error::File)));
        assert!(alt_os::temp::emptied(&link).is_err());
        assert!(elsewhere.join("kept").exists());
    }
}
