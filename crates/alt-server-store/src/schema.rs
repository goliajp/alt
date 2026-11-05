use crate::{ByteStore, Error, InstanceSpec, Result, Store};
use alt_remote_protocol::Limits;
use ed25519_dalek::SigningKey;
use postgres::{Client, GenericClient};

const MIGRATIONS: [&str; 2] = [include_str!("schema.sql"), include_str!("schema-v2.sql")];
pub(crate) fn check(client: &mut impl GenericClient, expected: usize) -> Result<()> {
    let rows = client.query(
        "SELECT version,checksum FROM alt_schema ORDER BY version",
        &[],
    )?;
    if rows.len() != expected
        || rows.iter().enumerate().any(|(index, row)| {
            row.get::<_, i32>(0) != (index + 1) as i32
                || row.get::<_, String>(1)
                    != blake3::hash(MIGRATIONS[index].as_bytes()).to_hex().as_str()
        })
    {
        return Err(Error::Precondition(
            "unsupported or altered database schema; explicit migration required",
        ));
    }
    Ok(())
}
pub(crate) fn apply_v2(client: &mut impl GenericClient) -> Result<()> {
    client.batch_execute(MIGRATIONS[1])?;
    client.execute(
        "INSERT INTO alt_schema VALUES(2,$1)",
        &[&blake3::hash(MIGRATIONS[1].as_bytes()).to_hex().to_string()],
    )?;
    Ok(())
}
impl Store {
    /// Explicit operator migration of a stopped v1 instance. Identity and both
    /// migration checksums are validated; runtime open never migrates silently.
    pub fn migrate_v1(
        mut client: Client,
        bytes: ByteStore,
        mut spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
    ) -> Result<Self> {
        limits.validate()?;
        let mut tx = client.transaction()?;
        tx.query_one("SELECT pg_advisory_xact_lock(715071909111)", &[])?;
        check(&mut tx, 1)?;
        tx.query_one(
            "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
            &[],
        )?;
        crate::store::instance(&mut tx, &spec, &signing_key)?;
        tx.batch_execute("LOCK TABLE alt_repositories,alt_requests IN ACCESS EXCLUSIVE MODE")?;
        apply_v2(&mut tx)?;
        tx.execute("UPDATE alt_requests SET fence=fence+1,worker_lease_until=0 WHERE state IN ('receiving','ready')", &[])?;
        spec.runtime_generation = spec
            .runtime_generation
            .checked_add(1)
            .ok_or(Error::Limit("runtime generation"))?;
        tx.execute(
            "UPDATE alt_instance SET runtime_generation=$1 WHERE singleton",
            &[&crate::store::integer(spec.runtime_generation)?],
        )?;
        tx.commit().map_err(|_| Error::OutcomeUnknown)?;
        Self::open(client, bytes, spec, signing_key, limits)
    }
}
