use crate::{Error, PreparedTaskGraph, RecordKey, RecordKind, Result, TaskRecord};

impl PreparedTaskGraph {
    fn linked(&self, kind: RecordKind, id: &str) -> Result<&TaskRecord> {
        self.record(&RecordKey {
            kind,
            id: id.into(),
        })
    }
    /// Check relationships which individual record hashes cannot establish.
    /// Evidence discrepancies remain received facts for acceptance evaluation.
    pub fn verify_record_links(&self) -> Result<()> {
        for record in self.records.values() {
            match record {
                TaskRecord::Candidate(candidate) => {
                    let TaskRecord::Task(task) = self.linked(RecordKind::Task, &candidate.task)?
                    else {
                        return Err(Error::Integrity("candidate task kind"));
                    };
                    if candidate.target_expected != task.target_expected {
                        return Err(Error::Integrity("candidate target binding"));
                    }
                    if let Some(parent) = &candidate.parent {
                        let TaskRecord::Candidate(parent) =
                            self.linked(RecordKind::Candidate, parent)?
                        else {
                            return Err(Error::Integrity("candidate parent kind"));
                        };
                        if parent.task != candidate.task {
                            return Err(Error::Integrity(
                                "candidate parent belongs to another task",
                            ));
                        }
                    }
                }
                TaskRecord::Review(review) => {
                    let TaskRecord::Candidate(candidate) =
                        self.linked(RecordKind::Candidate, &review.candidate)?
                    else {
                        return Err(Error::Integrity("review candidate kind"));
                    };
                    let TaskRecord::Evidence(evidence) =
                        self.linked(RecordKind::Evidence, &review.evidence)?
                    else {
                        return Err(Error::Integrity("review evidence kind"));
                    };
                    let TaskRecord::TaskBaseline(baseline) =
                        self.linked(RecordKind::TaskBaseline, &review.baseline)?
                    else {
                        return Err(Error::Integrity("review baseline kind"));
                    };
                    if candidate.task != review.task
                        || evidence.candidate != review.candidate
                        || baseline.task != review.task
                        || review.target_expected != candidate.target_expected
                    {
                        return Err(Error::Integrity("review record binding"));
                    }
                }
                TaskRecord::Integration(integration) => {
                    let TaskRecord::Review(review) =
                        self.linked(RecordKind::Review, &integration.review)?
                    else {
                        return Err(Error::Integrity("integration review kind"));
                    };
                    if integration.task != review.task
                        || review.target_expected.get(&integration.target)
                            != Some(&integration.before)
                    {
                        return Err(Error::Integrity("integration review target binding"));
                    }
                }
                TaskRecord::Delivery(delivery) => {
                    let TaskRecord::Integration(integration) =
                        self.linked(RecordKind::Integration, &delivery.integration)?
                    else {
                        return Err(Error::Integrity("delivery integration kind"));
                    };
                    if integration.task != delivery.task {
                        return Err(Error::Integrity("delivery task binding"));
                    }
                }
                TaskRecord::Event(event) => {
                    if let Some(previous) = &event.previous {
                        let TaskRecord::Event(previous) =
                            self.linked(RecordKind::Event, previous)?
                        else {
                            return Err(Error::Integrity("event predecessor kind"));
                        };
                        if previous.task != event.task {
                            return Err(Error::Integrity(
                                "event predecessor belongs to another task",
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
