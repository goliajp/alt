//! The empty directories a move takes along, kept beside the log and not
//! in it. A program that knows nothing of such a change would write a
//! file where the directory is to be, and one that met a record it does
//! not know would write nothing more to the store. So the intent in the
//! log is of the files alone, which every program finishes and takes back
//! as it is, and the directories are in a file named by that record: this
//! program reads the two as one intent, and another never looks there.

use super::super::*;
use super::{CheckoutIntent, EMPTY_DIRECTORY, FileChange, FileState, PREPARE_CHECKOUT};
use recovery::{decode, durable_write, encode, hash};

const KEPT: &str = "moved-empty";

#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
struct Hollow {
    version: u32,
    files: Vec<FileChange>,
}

fn hollow(state: &Option<FileState>) -> bool {
    state
        .as_ref()
        .is_some_and(|state| state.mode == EMPTY_DIRECTORY)
}

fn of_directory(change: &FileChange) -> bool {
    hollow(&change.before) || hollow(&change.after)
}

/// `intent` without the empty directories it changes.
fn of_files(intent: &CheckoutIntent) -> CheckoutIntent {
    let files = intent.files.iter().filter(|c| !of_directory(c));
    CheckoutIntent {
        files: files.cloned().collect(),
        ..intent.clone()
    }
}

impl NativeRepo<'_> {
    fn hollow_place(&self, record: &[u8]) -> PathBuf {
        self.index_path.with_file_name(KEPT).join(hash(record))
    }

    /// The record of `intent` for the log, with the empty directories it
    /// changes kept apart under the name of that record: written before
    /// the record is, so that whoever finds the record finds them.
    pub(in crate::native) fn checkout_record(&self, intent: &CheckoutIntent) -> Res<Vec<u8>> {
        if !intent.files.iter().any(of_directory) {
            let record = encode(PREPARE_CHECKOUT, intent)?;
            // what an earlier intent of the same bytes kept is not of this one
            return match std::fs::remove_file(self.hollow_place(&record)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(record),
            };
        }
        let apart = intent.files.iter().filter(|c| of_directory(c));
        let apart: Vec<FileChange> = apart.cloned().collect();
        // an empty directory comes or goes, and is nothing else before or after
        if apart
            .iter()
            .any(|c| c.before.is_some() && c.after.is_some())
        {
            return Err("an empty directory cannot change into a file".into());
        }
        let record = encode(PREPARE_CHECKOUT, &of_files(intent))?;
        let kept = Hollow {
            version: 1,
            files: apart,
        };
        durable_write(&self.hollow_place(&record), &alt_json::to_vec(&kept)?)?;
        Ok(record)
    }

    /// The intent `record` is of, with the empty directories kept for it.
    pub(in crate::native) fn checkout_recorded(&self, record: &[u8]) -> Res<CheckoutIntent> {
        let mut intent: CheckoutIntent = decode(record, PREPARE_CHECKOUT)?;
        let kept = match std::fs::read(self.hollow_place(record)) {
            Ok(kept) => kept,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(intent),
            Err(e) => return Err(e.into()),
        };
        let kept: Hollow = alt_json::from_slice(&kept)?;
        if kept.version != 1 {
            return Err(crate::machine::Failure::refusal(
                "unsupported_record",
                "unsupported record of moved directories",
            )
            .into());
        }
        intent.files.extend(kept.files);
        Ok(intent)
    }

    /// Removes what was kept for the intents before `intent`, which is
    /// done: only the last operation is taken back, and none before it is
    /// left to finish.
    pub(super) fn forget_moved_before(&self, intent: &CheckoutIntent) -> Res<()> {
        let kept = match std::fs::read_dir(self.index_path.with_file_name(KEPT)) {
            Ok(kept) => kept,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let mut last = None;
        for entry in kept {
            let entry = entry?;
            if last.is_none() {
                last = Some(hash(&encode(PREPARE_CHECKOUT, &of_files(intent))?));
            }
            if entry.file_name().to_str() != last.as_deref() {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
}
