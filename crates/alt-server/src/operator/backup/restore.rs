//! Bringing a verified backup into an empty installation.
use super::unread::Unread;
use super::*;

/// Restore only into an empty, dedicated DB and absent volume/key paths. The
/// target config already carries the operator-selected new runtime generation,
/// so it cannot serve the intermediate old-generation SQL archive.
///
/// A restoration that fails before the instance is activated has made nothing
/// that can serve: what it wrote is taken away again, and it can be begun
/// again. One that fails at the activation or after it is left as it is.
pub fn restore(
    target: &Config,
    backup: VerifiedBackup,
    source_fenced: bool,
    with_unread: bool,
) -> Result<(SnapshotCatalog, Vec<Unread>)> {
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
    let outcome = brought_in(
        target,
        &backup,
        cloud.as_ref(),
        client,
        with_unread,
        &mut stage,
    );
    let error = match outcome {
        Ok(restored) => return Ok(restored),
        Err(error) => error,
    };
    // The destination may now contain durable state, even if a later check or
    // connection failed. Never label a partially applied restoration not_applied.
    if matches!(stage, ACTIVATION | "final health verification") || undone(target).is_err() {
        return Err(Error::RestoreStage(stage));
    }
    Err(match error {
        told @ Error::Partly(..) => told,
        _ => Error::RestoreUndone(stage),
    })
}

const ACTIVATION: &str = "recovery activation";

/// Takes away what a restoration that was not activated wrote: the files, which
/// were absent before it, and what the archive made in the database, which had
/// nothing before it.
fn undone(target: &Config) -> Result<()> {
    let absent = |removed: std::io::Result<()>| match removed {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(Error::File),
        _ => Ok(()),
    };
    absent(std::fs::remove_file(&target.receipt_key_file))?;
    absent(std::fs::remove_dir_all(&target.bytes_directory))?;
    let mut client = target.client()?;
    client.batch_execute(EMPTIED).map_err(|_| Error::Database)
}

/// Drops what the schema of the archive holds. The names are gathered before the
/// first of them is dropped: one that goes with another is gone when it is come to.
/// The sequences come last: one that is a column's own goes with its table and
/// is not dropped by itself.
const EMPTIED: &str = "DO $$ DECLARE made text; BEGIN \
FOR made IN SELECT * FROM unnest(ARRAY(SELECT format('DROP %s IF EXISTS %s CASCADE', CASE c.relkind WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW' WHEN 'S' THEN 'SEQUENCE' WHEN 'f' THEN 'FOREIGN TABLE' ELSE 'TABLE' END, c.oid::regclass) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relkind IN ('r','p','v','m','S','f') ORDER BY c.relkind='S')) LOOP EXECUTE made; END LOOP; \
FOR made IN SELECT * FROM unnest(ARRAY(SELECT format('DROP %s IF EXISTS %s CASCADE', CASE p.prokind WHEN 'p' THEN 'PROCEDURE' WHEN 'a' THEN 'AGGREGATE' ELSE 'FUNCTION' END, p.oid::regprocedure) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='public')) LOOP EXECUTE made; END LOOP; \
FOR made IN SELECT * FROM unnest(ARRAY(SELECT format('DROP TYPE IF EXISTS %s CASCADE', t.oid::regtype) FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname='public' AND t.typtype IN ('c','d','e','r') AND (t.typrelid=0 OR EXISTS(SELECT 1 FROM pg_class c WHERE c.oid=t.typrelid AND c.relkind='c')))) LOOP EXECUTE made; END LOOP; \
END $$";

/// The files of the backup written where the target has them.
fn files(target: &Config, backup: &VerifiedBackup) -> Result<ByteStore> {
    let original = &backup.manifest.catalog;
    // no file is written over: the destination was found without them
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
    Ok(bytes)
}

/// The archive of the database brought in, and what it tells held against the
/// signed snapshot and the files.
fn database(
    target: &Config,
    backup: &VerifiedBackup,
    client: &mut alt_pg::Client,
    bytes: &ByteStore,
    stage: &mut &'static str,
) -> Result<()> {
    let original = &backup.manifest.catalog;
    *stage = "database archive";
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
    *stage = "restored snapshot verification";
    let key = &target.signing_key()?;
    MaintenanceSnapshot::normalize_restored_locations(client, bytes, original, key)?;
    {
        let snapshot = MaintenanceSnapshot::begin(
            client,
            bytes.clone(),
            &original.instance,
            key,
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
    Ok(())
}

/// What a restoration does to a destination that was found empty. `stage` is
/// what it was at when it failed.
fn brought_in(
    target: &Config,
    backup: &VerifiedBackup,
    cloud: Option<&std::sync::Arc<crate::cloud::Runtime>>,
    mut client: alt_pg::Client,
    with_unread: bool,
    stage: &mut &'static str,
) -> Result<(SnapshotCatalog, Vec<Unread>)> {
    let original = &backup.manifest.catalog;
    let bytes = files(target, backup)?;
    database(target, backup, &mut client, &bytes, stage)?;
    let key = target.signing_key()?;
    *stage = "schema migration";
    let mut store = if original.schema == alt_server_store::SCHEMA_VERSION {
        alt_server_store::Store::open(client, bytes, original.instance.clone(), key, target.limits)?
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
    *stage = "restored objects read";
    let unread = super::unread::found(&mut store)?;
    if !with_unread && !super::unread::told_of(&unread, &backup.manifest.unread) {
        let hint = super::unread::RESTORED;
        return Err(super::unread::refused("restored", &unread, hint));
    }
    *stage = ACTIVATION;
    if let Some(cloud) = cloud {
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
    *stage = "final health verification";
    target.open()?;
    let mut client = target.client()?;
    let snapshot = MaintenanceSnapshot::begin(
        &mut client,
        ByteStore::open(&target.bytes_directory)?,
        &target.instance,
        &target.signing_key()?,
        alt_server_store::SCHEMA_VERSION,
    )?;
    Ok((snapshot.catalog().clone(), unread))
}
