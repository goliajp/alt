use super::*;

/// Limits include duplicate inputs: staging never opens an unbounded set of files.
pub const MAX_PUT_INPUTS: usize = 128;
pub const MAX_PUT_BYTES: u64 = 64 * 1024 * 1024;

/// One streaming input, with its declared identity checked before installation.
pub struct ByteInput<'a> {
    pub digest: &'a str,
    pub bytes: u64,
    pub source: &'a mut dyn Read,
}

// Each request already bounds its staged files. Overlap only their independent
// confirmations, and join every started worker even after a failure. No caller
// may install files or return proofs while a previous phase is still running.
fn synchronize<T: Sync>(
    values: &[T],
    sync: impl Fn(&T) -> std::io::Result<()> + Sync,
) -> std::io::Result<()> {
    if values.len() <= 1 {
        return values.iter().try_for_each(sync);
    }
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut result = Ok(());
        let sync = &sync;
        for chunk in values.chunks(values.len().div_ceil(4)) {
            match std::thread::Builder::new()
                .spawn_scoped(scope, move || chunk.iter().try_for_each(sync))
            {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        for handle in handles {
            let completed = handle.join().unwrap_or_else(|_| {
                Err(std::io::Error::other(
                    "file synchronization worker panicked",
                ))
            });
            if result.is_ok() {
                result = completed;
            }
        }
        result
    })
}

impl ByteStore {
    /// Stage a bounded group, sync every temporary file, install without
    /// replacement, sync every installed file, then sync the shared directory.
    /// A failure returns no proofs; already installed orphan bytes are harmless
    /// and must still pass ordinary registration before becoming visible.
    pub fn put_batch(
        &self,
        scope: &Scope,
        inputs: &mut [ByteInput<'_>],
        limit: u64,
    ) -> Result<Vec<DurableBytes>> {
        crate::telemetry::scope(scope);
        let mut phase =
            alt_server_telemetry::PhaseGuard::current(alt_server_telemetry::Phase::Persist);
        let result = self.put_batch_inner(scope, inputs, limit);
        if let Ok(proofs) = &result {
            phase.add_bytes(proofs.iter().map(DurableBytes::bytes).sum());
        }
        phase.finish(
            result
                .as_ref()
                .map(|_| alt_server_telemetry::Outcome::Ok)
                .unwrap_or_else(crate::telemetry::outcome),
        );
        result
    }

    fn put_batch_inner(
        &self,
        scope: &Scope,
        inputs: &mut [ByteInput<'_>],
        limit: u64,
    ) -> Result<Vec<DurableBytes>> {
        scope.validate()?;
        if inputs.is_empty() || inputs.len() > MAX_PUT_INPUTS {
            return Err(Error::Limit("byte batch count"));
        }
        let mut total = 0u64;
        for input in inputs.iter() {
            check_digest(input.digest)?;
            total = total
                .checked_add(input.bytes)
                .ok_or(Error::Limit("byte batch size"))?;
            if input.bytes > limit || total > MAX_PUT_BYTES {
                return Err(Error::Limit("byte batch size"));
            }
        }
        let parent = self.repository(scope)?;
        let staged = inputs
            .iter_mut()
            .map(|input| stage(&parent, input.digest, input.bytes, input.source))
            .collect::<Result<Vec<_>>>()?;
        synchronize(&staged, |temp| temp.as_file().sync_all())?;
        let mut proofs = Vec::with_capacity(inputs.len());
        for (input, temp) in inputs.iter().zip(staged) {
            let file = match temp.persist_noclobber(parent.join(input.digest)) {
                Ok(file) => file,
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    self.verify(scope, input.digest, input.bytes)?
                }
                Err(error) => return Err(error.error.into()),
            };
            proofs.push(DurableBytes {
                file: DurableFile::Installed(Arc::new(file)),
                location: None,
                segment_checksum: None,
                scope: scope.clone(),
                blake3: input.digest.into(),
                bytes: input.bytes,
            });
        }
        synchronize(&proofs, |proof| match &proof.file {
            DurableFile::Installed(file) => file.sync_all(),
            DurableFile::Existing(_) => Err(std::io::Error::other("expected installed byte proof")),
        })?;
        File::open(parent)?.sync_all()?;
        for proof in &proofs {
            self.check_proof(proof)?;
        }
        Ok(proofs)
    }
}

#[cfg(test)]
mod tests {
    use super::synchronize;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Duration;

    #[test]
    fn synchronization_visits_each_input_once_with_bounded_workers() {
        let values: Vec<_> = (0..128).map(|_| AtomicUsize::new(0)).collect();
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        synchronize(&values, |visits| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(count, Ordering::SeqCst);
            visits.fetch_add(1, Ordering::SeqCst);
            std::thread::yield_now();
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert!(maximum.load(Ordering::SeqCst) <= 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(values.iter().all(|v| v.load(Ordering::SeqCst) == 1));
    }

    #[test]
    fn synchronization_joins_pending_workers_after_error_or_panic() {
        for panic in [false, true] {
            let (started_tx, started_rx) = mpsc::channel();
            let (failed_tx, failed_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let finished = Arc::new(AtomicUsize::new(0));
            let observed = finished.clone();
            let worker = std::thread::spawn(move || {
                let release_rx = Mutex::new(release_rx);
                let result = synchronize(&[0, 1, 2, 3], |value| {
                    if *value == 0 {
                        failed_tx.send(()).unwrap();
                        assert!(!panic, "injected synchronization worker panic");
                        return Err(std::io::Error::other("injected synchronization failure"));
                    }
                    if *value == 1 {
                        started_tx.send(()).unwrap();
                        release_rx.lock().unwrap().recv().unwrap();
                        observed.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(())
                });
                done_tx.send(result.is_err()).unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            failed_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
            release_tx.send(()).unwrap();
            assert!(done_rx.recv_timeout(Duration::from_secs(10)).unwrap());
            worker.join().unwrap();
            assert_eq!(finished.load(Ordering::SeqCst), 1);
        }
    }
}
