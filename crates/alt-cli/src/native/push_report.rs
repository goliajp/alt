//! What a push says of each ref it was asked to push.

use super::*;

/// Writes the outcome of a push that sent commands: what the server
/// answered to each, then the refs in `current`, which it already had at
/// the pushed value and so were not sent. `sent` is the number of objects
/// and of pack bytes.
pub(super) fn render(
    out: &mut impl Write,
    json: bool,
    remote_name: &str,
    sent: (usize, usize),
    commands: &[alt_wire::CommandStatus],
    current: &[alt_wire::RefUpdate],
) -> Res<()> {
    let (objects, pack_bytes) = sent;
    if json {
        use crate::json::Json;
        let answered = commands.iter().map(|s| match s {
            alt_wire::CommandStatus::Ok(name) => {
                Json::Object(vec![("name", Json::str(name)), ("status", Json::str("ok"))])
            }
            alt_wire::CommandStatus::Ng { name, reason } => Json::Object(vec![
                ("name", Json::str(name)),
                ("status", Json::str("ng")),
                ("reason", Json::str(reason)),
            ]),
        });
        let unsent = current.iter().map(|u| {
            Json::Object(vec![
                ("name", Json::str(&u.name)),
                ("status", Json::str("up-to-date")),
            ])
        });
        crate::json::emit(
            out,
            vec![
                ("remote", Json::str(remote_name)),
                ("objects", Json::Num(objects as i64)),
                ("pack_bytes", Json::Num(pack_bytes as i64)),
                ("refs", Json::Array(answered.chain(unsent).collect())),
            ],
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "push '{remote_name}': {objects} object(s), {pack_bytes} byte(s)"
    )?;
    for s in commands {
        match s {
            alt_wire::CommandStatus::Ok(name) => writeln!(out, "  ok       {name}")?,
            alt_wire::CommandStatus::Ng { name, reason } => {
                writeln!(out, "  rejected {name}: {reason}")?
            }
        }
    }
    for u in current {
        writeln!(out, "  up-to-date {}", u.name)?;
    }
    Ok(())
}
