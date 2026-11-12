use crate::{ByteStore, Error, InstanceSpec, Result, Store};
use alt_crypto::ed25519::SigningKey;
use alt_pg::{Client, GenericClient};
use alt_remote_protocol::Limits;

/// the whole current layout, for a database that is being created
const BASELINE: &str = include_str!("schema.sql");
/// what takes schema 18 to 19, ahead of the data step in `schema_v19`
const VERSION_19: &str = include_str!("schema-v19.sql");
/// what takes schema 19 to 20, ahead of the data step in `schema_v20`
const VERSION_20: &str = include_str!("schema-v20.sql");

/// The oldest schema this release reads: it upgrades an instance of that schema,
/// and takes and restores backups from it on.
pub const OLDEST_SCHEMA: u32 = 18;
pub const UNSUPPORTED_SOURCE_SCHEMA: &str = "unsupported source schema: this release upgrades from schema 18 and 19 only; upgrade an older instance to schema 18 with the last release that carries those migrations first";
const UNSUPPORTED_BACKUP_SCHEMA: &str = "unsupported schema: this release backs up and restores schema 18, 19 and 20 only; restore or upgrade an older instance to schema 18 with the last release that carries those migrations first";
/// The publication documents are stored with lz4 compression, which a PostgreSQL
/// server has only when it was built with it. Without it the first large document
/// would fail to be written.
pub const LZ4_REQUIRED: &str = "this PostgreSQL server was built without lz4 compression, which the schema requires; use a server built with lz4 (--with-lz4), as the official images are";

/// whether this release takes or restores a backup of an instance at `schema`
pub fn check_backup_schema(schema: u32) -> std::result::Result<(), &'static str> {
    if (OLDEST_SCHEMA..=crate::SCHEMA_VERSION).contains(&schema) {
        Ok(())
    } else {
        Err(UNSUPPORTED_BACKUP_SCHEMA)
    }
}

/// Checksums of the migrations that took a database to schema 18 one version at a
/// time, before the baseline replaced them. `alt_schema` has a row for every version,
/// and a database created from the baseline is given the same rows: it is the
/// database those migrations produce, so an instance cannot be told by its age, the
/// release before this one opens either, and a later upgrade meets one history.
const HISTORY: [&str; 18] = [
    "6fa2abb89f611058a11cc72fbedf380dd8816c0f844dda01728c79a21e156ac1",
    "750350f4b0563ebe41747e9442d2c19d2c6be694329038bed05fdef5594075c4",
    "fcd1d00af55a371805edbc5ab1bc95b81e2248c632efd78c0bda56919a64cb9f",
    "818e25d511ad5a9f286f97cf96be2397e91ac38163d6c4000e871d9f4b79cadf",
    "6c49d8448b8fec27ce58c167baf3e1f80ca159054aed128cb828994c2511d06d",
    "8b3888142bed7d4a0bad60ab28c45b77ac4c8cf8b5069bcd20067571e62fc38f",
    "aac72fd6fdfd3fd6c8f33b600a7d3a85176f697b99bcea532418423d4e1ee813",
    "057943e47ac575e8c76b76009375bbf68bd0d4bf6ba9e29f6acfb80e580d781a",
    "d5045ef37b2040dabd2b7417e9c76d7a0dd2c186eceba2374b5bbaaec0f20731",
    "2afe38f625418afb2d5a4aac7d1a8f8ccb05244bc4eadd64172879460a2e39f3",
    "5910c81ba45cb19e1dc4eebd904d4350387dadd04b570b3a122af56415c752c2",
    "69dd5aac3263a1803d63d68875046b7ce4448ad0bdc9f9fcb17ec75136284902",
    "2b4d0095e0e2d7eddd3c604a9acfdb2b633f61eba1c579795200656dbdfa39fa",
    "5703082e139474171105f845a6fa55a6823a92f1b98698195b15adb659f59ced",
    "93c180e9da850ae25bb275418519649c01bbffb6a21e02845842c485548f1340",
    "d688152c14c222e3f18285b7dfab74f44a16b717d0ca231633bb8e31369b72e2",
    "6375882fe2562555aca195ff7d1b2d78a272de7e6e55e55aa94734ee3b23ea19",
    "8692b68d6db224b2238688a2411f132b57833b459324d7ea84420c9d5c9b7fa8",
];

/// what `alt_schema` holds for each version up to the current one
fn checksums() -> Vec<String> {
    let mut all: Vec<String> = HISTORY.iter().map(|checksum| (*checksum).into()).collect();
    for version in [VERSION_19, VERSION_20] {
        all.push(
            alt_hash::blake3::hash(version.as_bytes())
                .to_hex()
                .to_string(),
        );
    }
    all
}
/// refuses a server that cannot write the compressed columns
pub(crate) fn require_lz4(client: &mut impl GenericClient) -> Result<()> {
    let supported: bool = client
        .query_one(
            "SELECT 'lz4'=ANY(enumvals) FROM pg_settings WHERE name='default_toast_compression'",
            &[],
        )?
        .get(0);
    if supported {
        Ok(())
    } else {
        Err(Error::Precondition(LZ4_REQUIRED))
    }
}
pub(crate) fn check(client: &mut impl GenericClient, expected: usize) -> Result<()> {
    let rows = client.query(
        "SELECT version,checksum FROM alt_schema ORDER BY version",
        &[],
    )?;
    check_rows(&rows, expected)
}
/// what opening an instance requires of its database
pub(crate) fn check_current(client: &mut impl GenericClient) -> Result<()> {
    check(client, crate::SCHEMA_VERSION as usize)?;
    require_lz4(client)
}
/// Every recorded version is held to its checksum, the ones the baseline replaced
/// included: a row that was changed or is missing is not the history this release
/// continues.
pub(crate) fn check_rows(rows: &[alt_pg::Row], expected: usize) -> Result<()> {
    let checksums = checksums();
    if rows.len() != expected
        || rows.iter().enumerate().any(|(index, row)| {
            row.get::<_, i32>(0) != (index + 1) as i32
                || checksums.get(index) != Some(&row.get::<_, String>(1))
        })
    {
        return Err(Error::Precondition(
            "unsupported or altered database schema; explicit migration required",
        ));
    }
    Ok(())
}
fn record(client: &mut impl GenericClient, version: usize, checksum: &str) -> Result<()> {
    client.execute(
        "INSERT INTO alt_schema VALUES($1,$2)",
        &[&(version as i32), &checksum],
    )?;
    Ok(())
}
/// the current schema in a database that has none
pub(crate) fn create(client: &mut impl GenericClient) -> Result<()> {
    require_lz4(client)?;
    client.batch_execute(BASELINE)?;
    for (index, checksum) in checksums().iter().enumerate() {
        record(client, index + 1, checksum)?;
    }
    Ok(())
}
impl Store {
    /// Explicit operator migration of a stopped instance from schema `from` to the
    /// current one, in one transaction. Identity and all migration checksums are
    /// validated; historical checksums, request identities and immutable content
    /// stay as they are. Runtime open never migrates silently.
    ///
    /// The sources are schema 18 and 19, and every version between the source and
    /// the current one is applied in turn. From 18 the published Git objects that
    /// version kept in the content store move into pack segments; see `schema_v19`.
    /// From 19 the stored state of committed requests and their outbox events give
    /// up their copy of the publication; see `schema_v20`.
    pub fn migrate(
        mut client: Client,
        bytes: ByteStore,
        mut spec: InstanceSpec,
        signing_key: SigningKey,
        limits: Limits,
        from: u32,
    ) -> Result<Self> {
        if !(OLDEST_SCHEMA..crate::SCHEMA_VERSION).contains(&from) {
            return Err(Error::Precondition(UNSUPPORTED_SOURCE_SCHEMA));
        }
        limits.validate()?;
        require_lz4(&mut client)?;
        // segment files the data step writes belong to rows of this transaction
        let mut written = Vec::new();
        let migrated: Result<u64> = (|| {
            let mut tx = crate::database::Transaction::uncached(client.transaction()?);
            tx.query_one("SELECT pg_advisory_xact_lock(715071909111)", &[])?;
            check(tx.raw_mut(), from as usize)?;
            tx.query_one(
                "SELECT singleton FROM alt_instance WHERE singleton FOR UPDATE",
                &[],
            )?;
            crate::store::instance(&mut tx, &spec, &signing_key)?;
            tx.batch_execute("LOCK TABLE alt_repositories,alt_requests IN ACCESS EXCLUSIVE MODE")?;
            if from == 18 {
                tx.batch_execute(VERSION_19)?;
                record(tx.raw_mut(), 19, &checksums()[18])?;
                crate::schema_v19::settle(&mut tx, &bytes, &spec.installation_id, &mut written)?;
            }
            tx.batch_execute(VERSION_20)?;
            record(tx.raw_mut(), 20, &checksums()[19])?;
            crate::schema_v20::shorten(&mut tx)?;
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
