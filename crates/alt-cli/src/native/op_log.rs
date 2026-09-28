//! `alt op-log --json` rendering.

use super::*;

/// `alt op-log --json` doc:
/// `{schema_version:1, ops:[{id, timestamp_ms, principal:{…}, verb,
/// ref_changes:[{name, old, new}] | null}]}`. `old`/`new` are `null` for
/// absent, `<oid>` for object targets, `@<name>` for symbolic targets.
pub(super) fn render_op_log_json(
    out: &mut impl Write,
    ops: &[&alt_oplog::Op],
    verdicts: &std::collections::HashMap<OpId, SigVerdict>,
) -> Res<()> {
    use crate::json::Json;
    let mut entries = Vec::with_capacity(ops.len());
    for op in ops {
        let (principal, verb) = Principal::parse_actor(&op.actor);
        let ref_changes = match parse_ref_tx_or_none(&op.payload)? {
            Some(tx) => Json::Array(
                tx.changes
                    .iter()
                    .map(|c| {
                        Json::Object(vec![
                            ("name", Json::str(&c.name)),
                            ("old", target_json(&c.old)),
                            ("new", target_json(&c.new)),
                        ])
                    })
                    .collect(),
            ),
            None => Json::Null,
        };
        let mut fields = vec![
            ("id", Json::str(hex32(&op.id.0))),
            ("timestamp_ms", Json::Num(op.timestamp_ms as i64)),
            ("principal", principal_json(&principal)),
            ("verb", Json::str(&verb)),
            ("ref_changes", ref_changes),
        ];
        if let Some(v) = verdicts.get(&op.id) {
            let (status, signer): (&'static str, Option<&str>) = match v {
                SigVerdict::Ok { principal } => ("signed-ok", Some(principal.as_str())),
                SigVerdict::Unsigned => ("unsigned", None),
                SigVerdict::Bad { principal } => ("bad-sig", Some(principal.as_str())),
                SigVerdict::Untrusted { principal } => ("untrusted", Some(principal.as_str())),
            };
            fields.push((
                "sig",
                Json::Object(vec![
                    ("status", Json::str(status)),
                    (
                        "principal",
                        match signer {
                            Some(s) => Json::str(s),
                            None => Json::Null,
                        },
                    ),
                ]),
            ));
        }
        entries.push(Json::Object(fields));
    }
    let doc = Json::Object(vec![
        ("schema_version", Json::Num(1)),
        ("ops", Json::Array(entries)),
    ]);
    doc.write(out)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn target_json(t: &Option<RefTarget>) -> crate::json::Json {
    use crate::json::Json;
    match t {
        None => Json::Null,
        Some(RefTarget::Oid(oid)) => Json::str(oid.to_string()),
        Some(RefTarget::Symbolic(name)) => Json::str(format!("@{name}")),
    }
}

fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}
