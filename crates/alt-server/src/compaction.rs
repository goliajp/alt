//! Merging pack segments after publications, on a thread of its own.
//!
//! A publication only announces its repository here and answers its client; the
//! merge, which rewrites stored bytes, costs the publisher nothing.

use crate::StoreFactory;
use alt_server_store::{PACK_RETIRED_GRACE_MS, Result, Store};
use alt_server_telemetry::BackgroundLog;
use std::{collections::BTreeSet, sync::mpsc};

#[derive(Clone)]
pub(crate) struct Compaction(mpsc::Sender<String>);

impl Compaction {
    /// The thread ends when the last clone of the returned handle is dropped.
    pub fn start(factory: StoreFactory) -> Self {
        let (sender, receiver) = mpsc::channel::<String>();
        let log = BackgroundLog::current();
        std::thread::Builder::new()
            .name("alt-compaction".into())
            .spawn(move || {
                let mut store = None;
                while let Ok(first) = receiver.recv() {
                    // several publications to one repository are one piece of work
                    let mut repositories = BTreeSet::from([first]);
                    repositories.extend(receiver.try_iter());
                    for repository in repositories {
                        if let Err(error) = compact(&factory, &mut store, &repository) {
                            log.failed("pack compaction", &repository, &error);
                            // the connection may be what failed
                            store = None;
                        }
                    }
                }
            })
            .expect("starting the compaction thread");
        Self(sender)
    }

    /// `repository` may have gained a segment
    pub fn published(&self, repository: &str) {
        // the receiver only goes away with the server
        let _ = self.0.send(repository.to_owned());
    }
}

fn compact(factory: &StoreFactory, store: &mut Option<Store>, repository: &str) -> Result<()> {
    let store = match store {
        Some(store) => store,
        None => store.insert(factory.open()?),
    };
    while store.compact_packs(repository)? {}
    store.sweep_packs(repository, PACK_RETIRED_GRACE_MS)?;
    Ok(())
}
