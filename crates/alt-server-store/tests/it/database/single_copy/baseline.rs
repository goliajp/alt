//! A new database is created at the current schema in one step, and only a schema 18
//! or 19 instance is upgraded to it.
use super::*;

/// what `alt_schema` records at schema 20, whether the database was created at it
/// or upgraded to it one version at a time
const VERSIONS: [&str; 20] = [
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
    "e16e814061ae7cc1f075bcebc6f231bfd9d07c9230b85304afa97766f38f8b68",
    "968c5f3709549ce9ab7b574bf8f60f67cff97ca3429cb9f6ef99c1f5800d4a8e",
];
/// the file a new database is created from; a change to it is a new schema version
const BASELINE: &str = "ea7dcb8951d50aa8184c6f0761cbd157aae0b0a5ff3af6abedebe64f52247540";

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_new_database_records_every_version_and_is_held_to_each() {
    assert_eq!(
        alt_hash::blake3::hash(include_bytes!("../../../../src/schema.sql"))
            .to_hex()
            .as_str(),
        BASELINE
    );
    let mut f = Fixture::new();
    let recorded: Vec<(i32, String)> = f
        .client()
        .query(
            "SELECT version,checksum FROM alt_schema ORDER BY version",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let expected: Vec<(i32, String)> = (1..)
        .zip(VERSIONS)
        .map(|(version, checksum)| (version, checksum.to_owned()))
        .collect();
    assert_eq!(recorded, expected);
    // a version the baseline replaced is checked like the current one
    for version in [1, 9, 17, 19, 20] {
        f.client()
            .execute(
                "UPDATE alt_schema SET checksum='altered' WHERE version=$1",
                &[&version],
            )
            .unwrap();
        assert!(f.store.check_connection().is_err(), "{version}");
        f.client()
            .execute(
                "UPDATE alt_schema SET checksum=$2 WHERE version=$1",
                &[&version, &VERSIONS[version as usize - 1]],
            )
            .unwrap();
        f.store.check_connection().unwrap();
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn an_instance_older_than_schema_18_is_refused_and_left_as_it_is() {
    let f = Fixture::new();
    let migrate = |from: u32| {
        Store::migrate(
            f.client(),
            f.store.bytes().clone(),
            f.store.spec().clone(),
            SigningKey::from_bytes(&f.signing_seed),
            limits(),
            from,
        )
        .map(|_| ())
    };
    // only the record matters here: the source is refused before anything is read
    f.client()
        .batch_execute("DELETE FROM alt_schema WHERE version>17")
        .unwrap();
    for from in [0, 1, 16, 17, 20, 21] {
        assert!(
            matches!(
                migrate(from),
                Err(alt_server_store::Error::Precondition(
                    alt_server_store::UNSUPPORTED_SOURCE_SCHEMA
                ))
            ),
            "{from}"
        );
    }
    // nor does naming it a supported schema get it upgraded
    for from in [18, 19] {
        assert!(matches!(
            migrate(from),
            Err(alt_server_store::Error::Precondition(
                "unsupported or altered database schema; explicit migration required"
            ))
        ));
    }
    let row = f
        .client()
        .query_one(
            "SELECT (SELECT max(version) FROM alt_schema),(SELECT runtime_generation FROM alt_instance)",
            &[],
        )
        .unwrap();
    assert_eq!((row.get::<_, i32>(0), row.get::<_, i64>(1)), (17, 1));
    assert!(alt_server_store::check_backup_schema(17).is_err());
    assert!(alt_server_store::check_backup_schema(18).is_ok());
    assert!(alt_server_store::check_backup_schema(19).is_ok());
    assert!(alt_server_store::check_backup_schema(20).is_ok());
    assert!(alt_server_store::check_backup_schema(21).is_err());
}
