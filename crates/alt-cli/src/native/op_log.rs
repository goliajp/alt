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
            (
                "request_id",
                requests::operation_request(op)?
                    .map(Json::str)
                    .unwrap_or(Json::Null),
            ),
            ("timestamp_ms", Json::Num(op.timestamp_ms as i64)),
            ("principal", principal_json(&principal)),
            ("verb", Json::str(&verb)),
            ("ref_changes", ref_changes),
        ];
        if let Some(tx) = parse_ref_tx_or_none(&op.payload)?
            && let Some(context) = tx.context
        {
            let context: recovery::RefContext = alt_json::from_slice(&context)?;
            fields.push((
                "workspace",
                Json::Object(vec![
                    ("name", Json::str(context.workspace.name)),
                    ("instance", Json::str(context.workspace.instance)),
                ]),
            ));
            fields.push((
                "inverse_of",
                context.inverse_of.map(Json::str).unwrap_or(Json::Null),
            ));
        }
        if op.payload.first() == Some(&task::TASK_OPERATION) {
            let operation: task::TaskOperation =
                recovery::decode(&op.payload, task::TASK_OPERATION)?;
            fields.push(("task_id", Json::str(operation.task_id)));
            fields.push(("record_id", Json::str(operation.record_id)));
            fields.push((
                "workspace",
                Json::Object(vec![
                    ("name", Json::str(operation.workspace.name)),
                    ("instance", Json::str(operation.workspace.instance)),
                ]),
            ));
        }
        if matches!(op.payload.first(), Some(0x30..=0x33)) {
            if op.payload.get(1) != Some(&1) {
                return Err("unsupported recovery record version".into());
            }
            let value: alt_json::Value = alt_json::from_slice(&op.payload[2..])?;
            let mut recovery = vec![(
                "phase",
                Json::str(if matches!(op.payload[0], 0x30 | 0x32) {
                    "prepared"
                } else {
                    "completed"
                }),
            )];
            for key in [
                "prepared",
                "before",
                "after",
                "index_before",
                "index_after",
                "inverse_of",
            ] {
                if let Some(value) = value.get(key).and_then(alt_json::Value::as_str) {
                    recovery.push((key, Json::str(value)));
                }
            }
            if let Some(workspace) = value.get("workspace") {
                recovery.push((
                    "workspace",
                    Json::Object(vec![
                        (
                            "name",
                            Json::str(workspace["name"].as_str().unwrap_or_default()),
                        ),
                        (
                            "instance",
                            Json::str(workspace["instance"].as_str().unwrap_or_default()),
                        ),
                    ]),
                ));
            }
            fields.push(("recovery", Json::Object(recovery)));
        }
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
