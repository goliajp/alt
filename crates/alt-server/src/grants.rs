//! The grants file of `alt-server grants set`.

#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(crate) struct Grants {
    pub(crate) read: bool,
    pub(crate) publish_records: bool,
    pub(crate) grants: Vec<alt_repository_service::Grant>,
}

#[cfg(test)]
mod json_tests {
    use super::*;

    #[test]
    fn recorded_verdicts() {
        alt_json_testkit::fixtures::verify_reader::<Grants>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json"),
            "main-Grants",
        );
    }
}
