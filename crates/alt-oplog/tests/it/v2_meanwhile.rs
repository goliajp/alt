//! What another handle does to a version 2 log while a writer has given
//! up the lock for whoever syncs what ops name: it appends, or looks at
//! what was left and cuts.

use std::path::Path;
use std::sync::{Arc, Mutex};

use alt_oplog::{Anchor, Discarded, Meta, Op, OpLog, OpLogError, SyncNamed, Unconfirmed};

/// Vouches for every op but the one of the payload `refused`, and notes
/// in `asked` each op it is asked about with its answer.
pub fn says<'a>(
    refused: &'a Option<Vec<u8>>,
    asked: &'a mut Vec<(Vec<u8>, bool)>,
) -> impl FnMut(&Op) -> Result<bool, OpLogError> + 'a {
    move |op| {
        let held = refused.as_ref() != Some(&op.payload);
        asked.push((op.payload.clone(), held));
        Ok(held)
    }
}

pub enum Plan {
    Append {
        meta: Meta,
        payload: Vec<u8>,
        unsynced: bool,
    },
    Look {
        refused: Option<Vec<u8>>,
    },
}

pub enum Outcome {
    Appended {
        meta: Meta,
        payload: Vec<u8>,
        unsynced: bool,
        head: Anchor,
        told: Vec<Discarded>,
    },
    /// a look that came to a cut the log refused
    Refused {
        refused: Option<Vec<u8>>,
        asked: Vec<(Vec<u8>, bool)>,
        why: OpLogError,
        told: Vec<Discarded>,
    },
    Looked {
        refused: Option<Vec<u8>>,
        asked: Vec<(Vec<u8>, bool)>,
        cut: Option<Unconfirmed>,
        told: Vec<Discarded>,
    },
    Failed(String),
}

/// What is to be done meanwhile the next time, and what was.
#[derive(Clone, Default)]
pub struct Meanwhile {
    plan: Arc<Mutex<Option<Plan>>>,
    done: Arc<Mutex<Vec<Outcome>>>,
}

impl Meanwhile {
    pub fn plan(&self, plan: Option<Plan>) {
        *self.plan.lock().unwrap() = plan;
    }

    pub fn done(&self) -> Vec<Outcome> {
        self.done.lock().unwrap().drain(..).collect()
    }

    /// Does what is planned, once, through a handle opened for it on the
    /// log in `dir` that has `seer` sync what ops name.
    pub fn run(&self, dir: &Path, seer: &SyncNamed) {
        let Some(plan) = self.plan.lock().unwrap().take() else {
            return;
        };
        let mut log = OpLog::open(dir).expect("the log opens");
        log.sync_named_with(seer.clone());
        let outcome = match plan {
            Plan::Append {
                meta,
                payload,
                unsynced,
            } => {
                let left = unsynced.then(|| alt_os::durability::leave_unsynced(true));
                let appended = log.append_with("meanwhile", 1, &payload, &meta);
                drop(left);
                match appended {
                    Ok(_) => Outcome::Appended {
                        meta,
                        payload,
                        unsynced,
                        head: log.anchor().unwrap(),
                        told: log.take_discarded(),
                    },
                    Err(error) => Outcome::Failed(format!("{error:?}")),
                }
            }
            Plan::Look { refused } => {
                let mut asked = Vec::new();
                match log.settle(says(&refused, &mut asked)) {
                    Ok(cut) => Outcome::Looked {
                        refused,
                        asked,
                        cut,
                        told: log.take_discarded(),
                    },
                    Err(why @ (OpLogError::Kept { .. } | OpLogError::Lost { .. })) => {
                        Outcome::Refused {
                            refused,
                            asked,
                            why,
                            told: log.take_discarded(),
                        }
                    }
                    Err(error) => Outcome::Failed(format!("{error:?}")),
                }
            }
        };
        self.done.lock().unwrap().push(outcome);
    }
}
