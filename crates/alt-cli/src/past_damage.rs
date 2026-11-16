//! `ALT_READ_PAST_DAMAGE`: a command that only reads goes on past damaged
//! records instead of failing at them, to get out what is left of a
//! repository (see [`alt_odb::reading_past_damage`]). It answers from
//! what reads, changes no file, and says on stderr which bytes of which
//! file it passed over.

use crate::cli::Command;
use crate::daemon::Response;
use crate::machine::Failure;

/// Refuses the commands that are not to read past damage: one that writes
/// to the repository would write after records it did not read, and the
/// full check is to report the repository as it is. An export writes
/// elsewhere. For every other command, makes the
/// process run it itself: a daemon holds its store open for writers too.
pub fn admit(command: &Command) -> Result<(), Failure> {
    if !alt_odb::reading_past_damage() {
        return Ok(());
    }
    let reads = matches!(command, Command::Export { .. }) || !crate::cli::mutates_command(command);
    if !reads || matches!(command, Command::Fsck { .. }) {
        return Err(Failure::usage(
            "ALT_READ_PAST_DAMAGE is for commands that only read, and not for fsck: unset it",
        ));
    }
    crate::client::stay_direct();
    Ok(())
}

/// Puts before what `response` says on stderr which bytes the command
/// passed over as damaged.
pub fn report(mut response: Response) -> Response {
    let mut passed = alt_odb::passed_damage();
    if let Some((file, from, to)) = alt_oplog::passed_damage() {
        passed.push(alt_odb::PassedDamage { file, from, to });
    }
    let mut said = String::new();
    for damage in passed {
        said.push_str(&format!(
            "warning: passed over bytes {} to {} of {}: they do not read, and what they held \
             is left out\n",
            damage.from,
            damage.to,
            damage.file.display()
        ));
    }
    response.stderr.splice(0..0, said.into_bytes());
    response
}
