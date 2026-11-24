//! The log a handle is on, for whoever reads or writes the note beside
//! it.

use crate::v2::log::Log;
use crate::v2::tail::Place;

impl Log {
    pub(super) fn beside(&self) -> Place<'_> {
        Place {
            dir: &self.dir,
            boot: self.boot,
            id: self.layout.id,
            log: &self.file,
        }
    }
}
