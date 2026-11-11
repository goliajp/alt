//! The grants file of `alt-server grants set`.

#[derive(serde::Deserialize, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub(crate) struct Grants {
    pub(crate) read: bool,
    pub(crate) publish_records: bool,
    pub(crate) grants: Vec<alt_repository_service::Grant>,
}

#[cfg(test)]
mod json_tests {
    use alt_json_testkit::arbitrary::from_serde;
    use alt_json_testkit::drive::exercise_reader;
    use alt_json_testkit::fixtures::{check_recorded, record_reader, verify_reader};
    use alt_json_testkit::rng::Rng;

    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");

    fn grants(rng: &mut Rng) -> alt_json::Value {
        let grants: Vec<alt_repository_service::Grant> =
            (0..rng.below(3)).map(|_| from_serde(rng)).collect();
        alt_json::json!({"read": rng.chance(2), "publish_records": rng.chance(2), "grants": grants})
    }

    fn view(value: &Grants) -> String {
        format!(
            "{} {} {}",
            value.read,
            value.publish_records,
            alt_json::to_string(&value.grants).unwrap()
        )
    }

    #[test]
    fn same_as_serde() {
        exercise_reader::<Grants>(0xd300, 300, grants, view).report("grants file");
    }

    #[test]
    fn recorded_verdicts() {
        check_recorded(
            &format!("{DIR}/main-Grants.txt"),
            &record_reader::<Grants>(0xd301, 15, grants),
        );
        verify_reader::<Grants>(DIR, "main-Grants");
    }
}
