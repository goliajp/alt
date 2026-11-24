//! Bringing a verified backup into an empty installation.
use super::*;

/// Restore only into an empty, dedicated DB and absent volume/key paths. The
/// target config already carries the operator-selected new runtime generation,
/// so it cannot serve the intermediate old-generation SQL archive.
pub fn restore(
    target: &Config,
    backup: VerifiedBackup,
    source_fenced: bool,
) -> Result<SnapshotCatalog> {
    if !source_fenced {
        return Err(Error::Config(
            "source must be externally fenced before disaster recovery",
        ));
    }
    target.validate()?;
    let cloud = target.cloud_runtime()?;
    let original = &backup.manifest.catalog;
    if target.instance.installation_id != original.instance.installation_id
        || target.instance.workspace_id != original.instance.workspace_id
        || target.instance.deployment_mode != original.instance.deployment_mode
        || target.instance.runtime_generation
            <= original.instance.runtime_generation.saturating_add(1)
        || original
            .repositories
            .iter()
            .any(|r| r.recovery_epoch >= target.instance.runtime_generation)
    {
        return Err(Error::Config(
            "restore identity or generation; use portable migration for another installation",
        ));
    }
    if target
        .bytes_directory
        .try_exists()
        .map_err(|_| Error::File)?
        || target
            .receipt_key_file
            .try_exists()
            .map_err(|_| Error::File)?
    {
        return Err(Error::Config(
            "restore requires absent target volume and receipt key",
        ));
    }
    let mut client = target.client()?;
    let acquired: bool = client
        .query_one("SELECT pg_try_advisory_lock(715071909111)", &[])
        .map_err(|_| Error::Database)?
        .get(0);
    if !acquired {
        return Err(Error::Config(
            "another target maintenance operation is active",
        ));
    }
    let empty: bool = client.query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname !~ '^pg_' AND n.nspname<>'information_schema')", &[]).map_err(|_| Error::Database)?.get(0);
    if !empty {
        return Err(Error::Config(
            "restore requires an empty dedicated database",
        ));
    }
    let mut stage = "destination files";
    let outcome = (|| -> Result<SnapshotCatalog> {
        // These no-clobber targets are deliberately left in place on failure. A
        // failed/unknown restoration is inspected or discarded as a whole, never
        // retried by overwriting an instance that might have accepted new writes.
        create_private(
            &target.receipt_key_file,
            &read_regular(&backup.path().join("receipt.key"), 32, true)?,
        )?;
        let bytes = ByteStore::open(&target.bytes_directory)?;
        create_private(
            &target.bytes_directory.join("instance.json"),
            &alt_json::to_vec(&target.volume_identity()?).map_err(|_| Error::File)?,
        )?;
        for blob in &original.blobs {
            let scope = alt_remote_protocol::Scope {
                installation_id: original.instance.installation_id.clone(),
                repository_instance_id: blob.repository.clone(),
                recovery_epoch: 1,
            };
            bytes.put(
                &scope,
                &blob.blake3,
                blob.bytes,
                blob.bytes,
                &mut regular(&backup.path().join(blob_path(blob)))?,
            )?;
        }
        for segment in &original.pack_segments {
            bytes.restore_pack_segment(
                &segment.repository,
                &segment.segment,
                segment.bytes,
                &segment.blake3,
                &mut regular(&backup.path().join(segment_path(segment)))?,
            )?;
        }
        stage = "database archive";
        let mut restore = Process(
            postgres_command(target, "pg_restore")?
                .args([
                    "--single-transaction",
                    "--exit-on-error",
                    "--clean",
                    "--if-exists",
                    "--no-owner",
                    "--no-privileges",
                ])
                // pg_restore otherwise defaults to writing a SQL script, even when
                // PGDATABASE is present. An empty conninfo uses that explicit env.
                .arg("--dbname=")
                .arg(backup.path().join("database.dump"))
                .spawn()
                .map_err(|_| Error::Config("pg_restore unavailable"))?,
        );
        checked(&mut restore)?;
        stage = "restored snapshot verification";
        let key = target.signing_key()?;
        MaintenanceSnapshot::normalize_restored_locations(&mut client, &bytes, original, &key)?;
        {
            let snapshot = MaintenanceSnapshot::begin(
                &mut client,
                bytes.clone(),
                &original.instance,
                &key,
                original.schema,
            )?;
            if snapshot.catalog() != original {
                return Err(Error::Config(
                    "restored catalog differs from signed snapshot",
                ));
            }
            for blob in &original.blobs {
                snapshot.read_verified(blob)?;
            }
        }
        stage = "schema migration";
        let mut store = if original.schema == alt_server_store::SCHEMA_VERSION {
            alt_server_store::Store::open(
                client,
                bytes,
                original.instance.clone(),
                key,
                target.limits,
            )?
        } else {
            alt_server_store::Store::migrate(
                client,
                bytes,
                original.instance.clone(),
                key,
                target.limits,
                original.schema,
            )?
        };
        stage = "recovery activation";
        if let Some(cloud) = &cloud {
            store.activate_restored_cloud(
                target.instance.runtime_generation,
                &backup.manifest_digest,
                alt_server_store::CloudRestoreBinding {
                    audience: cloud.audience(),
                    runtime_key_digest: cloud.key_digest(),
                    provision_digest: cloud.provision_digest(),
                },
            )?;
        } else {
            store.activate_restored(target.instance.runtime_generation, &backup.manifest_digest)?;
        }
        drop(store);
        stage = "final health verification";
        target.open()?;
        let mut client = target.client()?;
        let snapshot = MaintenanceSnapshot::begin(
            &mut client,
            ByteStore::open(&target.bytes_directory)?,
            &target.instance,
            &target.signing_key()?,
            alt_server_store::SCHEMA_VERSION,
        )?;
        Ok(snapshot.catalog().clone())
    })();
    // The destination may now contain durable state, even if a later check or
    // connection failed. Never label a partially applied restoration not_applied.
    outcome.map_err(|_| Error::RestoreStage(stage))
}
