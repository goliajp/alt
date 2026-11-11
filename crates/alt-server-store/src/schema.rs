use crate::{ByteStore, Error, InstanceSpec, Result, Store};
use alt_crypto::ed25519::SigningKey;
use alt_remote_protocol::Limits;
use postgres::{Client, GenericClient};

const MIGRATIONS: [&str; 19] = [
    include_str!("schema.sql"),
    include_str!("schema-v2.sql"),
    include_str!("schema-v3.sql"),
    include_str!("schema-v4.sql"),
    include_str!("schema-v5.sql"),
    include_str!("schema-v6.sql"),
    include_str!("schema-v7.sql"),
    include_str!("schema-v8.sql"),
    include_str!("schema-v9.sql"),
    include_str!("schema-v10.sql"),
    include_str!("schema-v11.sql"),
    include_str!("schema-v12.sql"),
    include_str!("schema-v13.sql"),
    include_str!("schema-v14.sql"),
    include_str!("schema-v15.sql"),
    include_str!("schema-v16.sql"),
    include_str!("schema-v17.sql"),
    include_str!("schema-v18.sql"),
    include_str!("schema-v19.sql"),
];
pub(crate) fn check(client: &mut impl GenericClient, expected: usize) -> Result<()> {
    let rows = client.query(
        "SELECT version,checksum FROM alt_schema ORDER BY version",
        &[],
    )?;
    check_rows(&rows, expected)
}
pub(crate) fn check_rows(rows: &[postgres::Row], expected: usize) -> Result<()> {
    if rows.len() != expected
        || rows.iter().enumerate().any(|(index, row)| {
            row.get::<_, i32>(0) != (index + 1) as i32
                || row.get::<_, String>(1)
                    != alt_hash::blake3::hash(MIGRATIONS[index].as_bytes())
                        .to_hex()
                        .as_str()
        })
    {
        return Err(Error::Precondition(
            "unsupported or altered database schema; explicit migration required",
        ));
    }
    Ok(())
}
/// every version after the first, for a database that was just created
pub(crate) fn apply_later(client: &mut impl GenericClient) -> Result<()> {
    (2..=MIGRATIONS.len()).try_for_each(|version| apply(client, version))
}
fn apply(client: &mut impl GenericClient, version: usize) -> Result<()> {
    client.batch_execute(MIGRATIONS[version - 1])?;
    client.execute(
        "INSERT INTO alt_schema VALUES($1,$2)",
        &[
            &(version as i32),
            &alt_hash::blake3::hash(MIGRATIONS[version - 1].as_bytes())
                .to_hex()
                .to_string(),
        ],
    )?;
    Ok(())
}
impl Store {
    /// Explicit operator migration of a stopped instance from schema `from` to the
    /// current one, in one transaction. Identity and all migration checksums are
    /// validated; historical checksums, request identities and immutable content
    /// stay as they are. Runtime open never migrates silently.
    ///
    /// From schema 18 or older this also moves the published Git objects that
    /// those versions kept in the content store into pack segments; see
    /// `schema_v19`.
    pub fn migrate(
        client: Client,
        bytes: ByteStore,
        spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
        from: u32,
    ) -> Result<Self> {
        if !(1..crate::SCHEMA_VERSION).contains(&from) {
            return Err(Error::Precondition("unsupported source schema"));
        }
        Self::migrate_from(client, bytes, spec, signing_key, limits, from as usize)
    }
    fn migrate_from(
        mut client: Client,
        bytes: ByteStore,
        mut spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
        from: usize,
    ) -> Result<Self> {
        limits.validate()?;
        // segment files the data step writes belong to rows of this transaction
        let mut written = Vec::new();
        let migrated: Result<u64> = (|| {
            let mut tx = crate::database::Transaction::uncached(client.transaction()?);
            tx.query_one("SELECT pg_advisory_xact_lock(715071909111)", &[])?;
            check(tx.raw_mut(), from)?;
            tx.query_one(
                "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
                &[],
            )?;
            crate::store::instance(&mut tx, &spec, &signing_key)?;
            tx.batch_execute("LOCK TABLE alt_repositories,alt_requests IN ACCESS EXCLUSIVE MODE")?;
            for version in from + 1..=MIGRATIONS.len() {
                apply(tx.raw_mut(), version)?;
            }
            crate::provenance::backfill_legacy(&mut tx, &spec, &signing_key, limits)?;
            crate::schema_v19::settle(&mut tx, &bytes, &spec.installation_id, &mut written)?;
            tx.execute("UPDATE alt_requests SET fence=fence+1,worker_lease_until=0 WHERE state IN ('receiving','ready')", &[])?;
            let generation = spec
                .runtime_generation
                .checked_add(1)
                .ok_or(Error::Limit("runtime generation"))?;
            tx.execute(
                "UPDATE alt_instance SET runtime_generation=$1 WHERE singleton",
                &[&crate::store::integer(generation)?],
            )?;
            tx.commit().map_err(|_| Error::OutcomeUnknown)?;
            Ok(generation)
        })();
        match migrated {
            Ok(generation) => spec.runtime_generation = generation,
            // the transaction may have committed: the sweep removes the files if it did not
            Err(Error::OutcomeUnknown) => return Err(Error::OutcomeUnknown),
            Err(error) => {
                for path in &written {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
        }
        Self::open(client, bytes, spec, signing_key, limits)
    }
}
