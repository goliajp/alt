use crate::{Json, connect};
use alt_pg::Error;

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn scalars_round_trip_at_their_limits() {
    let mut client = connect();
    let sql = "SELECT $1::int2,$2::int4,$3::int8,$4::bool,$5::text,$6::bytea,$7::oid,\
               $8::float4,$9::float8,$10::\"char\",$11::varchar,$12::name";
    let bytes: Vec<u8> = (0..=255).collect();
    for (small, medium, large, flag, text, data) in [
        (i16::MIN, i32::MIN, i64::MIN, false, "", &bytes[..0]),
        (
            i16::MAX,
            i32::MAX,
            i64::MAX,
            true,
            "héllo, 世界 \u{1f600}",
            &bytes[..],
        ),
        (
            0,
            -1,
            1,
            true,
            "it's \"quoted\" \\ and\nlined",
            &[0u8, 0, 0][..],
        ),
    ] {
        let row = client
            .query_one(
                sql,
                &[
                    &small,
                    &medium,
                    &large,
                    &flag,
                    &text,
                    &data,
                    &u32::MAX,
                    &1.5f32,
                    &-2.25f64,
                    &(b'x' as i8),
                    &text.to_owned(),
                    &"a_name",
                ],
            )
            .unwrap();
        assert_eq!(row.len(), 12);
        assert_eq!(row.get::<_, i16>(0), small);
        assert_eq!(row.get::<_, i32>(1), medium);
        assert_eq!(row.get::<_, i64>(2), large);
        assert_eq!(row.get::<_, bool>(3), flag);
        assert_eq!(row.get::<_, &str>(4), text);
        assert_eq!(row.get::<_, String>(4), text);
        assert_eq!(row.get::<_, &[u8]>(5), data);
        assert_eq!(row.get::<_, Vec<u8>>(5), data);
        assert_eq!(row.get::<_, u32>(6), u32::MAX);
        assert_eq!(row.get::<_, f32>(7), 1.5);
        assert_eq!(row.get::<_, f64>(8), -2.25);
        assert_eq!(row.get::<_, i8>(9), b'x' as i8);
        assert_eq!(row.get::<_, String>(10), text);
        assert_eq!(row.get::<_, &str>(11), "a_name");
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn nulls_are_options_in_both_directions() {
    let mut client = connect();
    let row = client
        .query_one(
            "SELECT $1::int8 AS number,$2::text AS words,$3::bytea AS data,$4::bool AS flag,\
             $5::jsonb AS doc,$6::int8 AS present",
            &[
                &None::<i64>,
                &None::<&str>,
                &None::<Vec<u8>>,
                &None::<bool>,
                &None::<Json>,
                &Some(5i64),
            ],
        )
        .unwrap();
    assert_eq!(row.get::<_, Option<i64>>("number"), None);
    assert_eq!(row.get::<_, Option<&str>>("words"), None);
    assert_eq!(row.get::<_, Option<String>>("words"), None);
    assert_eq!(row.get::<_, Option<Vec<u8>>>("data"), None);
    assert_eq!(row.get::<_, Option<bool>>("flag"), None);
    assert_eq!(row.get::<_, Option<Json>>("doc"), None);
    assert_eq!(row.get::<_, Option<i64>>("present"), Some(5));
    assert_eq!(row.get::<_, i64>(5), 5);
    assert!(matches!(
        row.try_get::<_, i64>("number"),
        Err(Error::Column { index: 0, .. })
    ));
    assert!(matches!(
        row.try_get::<_, i64>("absent"),
        Err(Error::NoColumn(_))
    ));
    assert!(matches!(row.try_get::<_, i64>(6), Err(Error::NoColumn(_))));
    assert!(matches!(
        row.try_get::<_, &str>("present"),
        Err(Error::Column { index: 5, .. })
    ));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn json_and_jsonb_carry_a_callers_own_type() {
    let mut client = connect();
    let document = Json(r#"{"a": [1, 2, {"b": null}], "c": "é"}"#.into());
    let row = client
        .query_one(
            "SELECT $1::jsonb,$1::jsonb->>'c',$2::json",
            &[&document, &document],
        )
        .unwrap();
    assert_eq!(row.get::<_, Json>(0), document);
    assert_eq!(row.get::<_, &str>(1), "é");
    assert_eq!(row.get::<_, Json>(2), document);
    assert!(matches!(
        client.query_one("SELECT $1::text", &[&document]),
        Err(Error::Parameter { index: 0, .. })
    ));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn arrays_are_sent_as_parameters() {
    let mut client = connect();
    let words = vec!["a".to_owned(), String::new(), "c,\"d\"".to_owned()];
    let numbers: &[i64] = &[3, -1, i64::MAX];
    let blobs = vec![vec![0u8, 1], vec![]];
    let row = client
        .query_one(
            "SELECT $1::text[]=ARRAY['a','','c,\"d\"'],\
             (SELECT sum(n)::text FROM unnest($2::int8[]) n),\
             $3::bytea[]=ARRAY['\\x0001'::bytea,''::bytea],\
             cardinality($4::int4[]),cardinality($5::bool[]),\
             'b'=ANY($6::text[]),array_position($7::text[],NULL)",
            &[
                &words,
                &numbers,
                &blobs,
                &Vec::<i32>::new(),
                &[true, false].as_slice(),
                &["a", "b"].as_slice(),
                &vec![Some("x"), None],
            ],
        )
        .unwrap();
    assert!(row.get::<_, bool>(0));
    assert_eq!(row.get::<_, &str>(1), "9223372036854775809");
    assert!(row.get::<_, bool>(2));
    assert_eq!(row.get::<_, i32>(3), 0);
    assert_eq!(row.get::<_, i32>(4), 2);
    assert!(row.get::<_, bool>(5));
    assert_eq!(row.get::<_, i32>(6), 2);
    assert!(matches!(
        client.query_one("SELECT $1::int8[]", &[&words]),
        Err(Error::Parameter { index: 0, .. })
    ));
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn parameters_must_match_the_statement() {
    let mut client = connect();
    assert!(matches!(
        client.query_one("SELECT $1::int4", &[&7i64]),
        Err(Error::Parameter {
            index: 0,
            ty: alt_pg::oid::INT4,
            ..
        })
    ));
    assert!(matches!(
        client.query_one("SELECT $1::int8,$2::text", &[&7i64, &8i64]),
        Err(Error::Parameter { index: 1, .. })
    ));
    assert!(matches!(
        client.query_one("SELECT $1::int8", &[]),
        Err(Error::ParameterCount {
            expected: 1,
            given: 0
        })
    ));
    assert!(matches!(
        client.query_one("SELECT 1", &[&1i64]),
        Err(Error::ParameterCount {
            expected: 0,
            given: 1
        })
    ));
    // nothing was sent for the refused calls
    assert!(!client.is_closed());
    assert_eq!(
        client
            .query_one("SELECT $1::int8", &[&7i64])
            .unwrap()
            .get::<_, i64>(0),
        7
    );
    // an inferred parameter type comes from the statement, not the value
    assert_eq!(
        client
            .query_one("SELECT length($1)", &[&"four"])
            .unwrap()
            .get::<_, i32>(0),
        4
    );
}
