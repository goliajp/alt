//! The record log under the map, as a whole.

use alt_store::StoreError;
use alt_store::reclog::RecLog;

use super::{MapEntry, ObjectMap, lift};
use crate::OdbError;

impl ObjectMap {
    /// All entries in append order.
    pub fn entries(&self) -> Result<Vec<MapEntry>, OdbError> {
        let mut out = Vec::with_capacity(self.log.count());
        let mut bad = None;
        self.log
            .for_each(|rec| -> Result<(), StoreError> {
                match MapEntry::parse(rec) {
                    Ok(entry) => out.push(entry),
                    Err(e) => bad = bad.take().or(Some(e)),
                }
                Ok(())
            })
            .map_err(lift)?;
        match bad {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    /// The log itself, for what is done to every record log alike.
    pub(crate) fn log_mut(&mut self) -> &mut RecLog {
        &mut self.log
    }
}
