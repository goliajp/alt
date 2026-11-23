//! Which commands run against the native store and which of them write.

use super::*;

/// Whether a command runs against the native `.alt` store (vs the git layer or
/// repo creation). `init` is neither — it creates a fresh repo.
pub fn is_native(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Task { .. }
            | Command::Candidate { .. }
            | Command::Evidence { .. }
            | Command::Request { .. }
            | Command::Add { .. }
            | Command::Commit { .. }
            | Command::Status { .. }
            | Command::Branch { .. }
            | Command::Tag { .. }
            | Command::CherryPick { .. }
            | Command::Revert { .. }
            | Command::Rebase { .. }
            | Command::Bisect { .. }
            | Command::Lfs { .. }
            | Command::Meta { .. }
            | Command::Switch { .. }
            | Command::Diff { .. }
            | Command::Merge { .. }
            | Command::Flow { .. }
            | Command::Undo { .. }
            | Command::Operation { .. }
            | Command::Workspace { .. }
            | Command::Remote { .. }
            | Command::Fetch { .. }
            | Command::Push { .. }
            | Command::Identity { .. }
            | Command::OpLog { .. }
            | Command::Verify { .. }
            | Command::Fsck(_)
            | Command::Complete(_)
            | Command::Prompt(_)
    )
}

pub fn mutates_command(cmd: &Command) -> bool {
    match cmd {
        Command::Task { op, .. } => {
            if let TaskOp::Remote { op } = op {
                if let TaskRemoteOp::Changes { op } = op {
                    return matches!(
                        op,
                        ChangeOp::Preview { .. }
                            | ChangeOp::Send { .. }
                            | ChangeOp::Reauthorize { .. }
                            | ChangeOp::RepairCreate { .. }
                    );
                }
                return !matches!(
                    op,
                    TaskRemoteOp::List { .. }
                        | TaskRemoteOp::Status { .. }
                        | TaskRemoteOp::Attest { .. }
                );
            }
            return matches!(
                op,
                TaskOp::Create { .. }
                    | TaskOp::Abandon { .. }
                    | TaskOp::Review { .. }
                    | TaskOp::Integrate { .. }
                    | TaskOp::IntegrationCollect { .. }
                    | TaskOp::Export { .. }
                    | TaskOp::DeliveryCollect { .. }
            );
        }
        Command::Candidate { op, .. } => return matches!(op, CandidateOp::Create { .. }),
        Command::Evidence { op, .. } => {
            return matches!(
                op,
                EvidenceOp::Import { .. } | EvidenceOp::Run { .. } | EvidenceOp::Collect { .. }
            );
        }
        _ => {}
    }
    if matches!(
        cmd,
        Command::Operation {
            op: OperationOp::Recover { .. } | OperationOp::Abandon { .. },
            ..
        }
    ) {
        return true;
    }
    if let Command::Config { value, unset, .. } = cmd {
        return value.is_some() || unset.is_some();
    }
    if !is_native(cmd) {
        return matches!(
            cmd,
            Command::Init { .. }
                | Command::Clone { .. }
                | Command::Import { .. }
                | Command::Intake { .. }
                | Command::Export { .. }
        );
    }
    native_mutates(cmd)
}

/// Whether a native command writes: all of them do but the pure reads.
fn native_mutates(cmd: &Command) -> bool {
    !matches!(
        cmd,
        Command::Request { .. }
            | Command::Operation { .. }
            | Command::Status { .. }
            | Command::Diff { .. }
            | Command::OpLog { .. }
            | Command::Verify { .. }
            | Command::Fsck(crate::fsck::FsckArgs { yes: false, .. })
            | Command::Complete(_)
            | Command::Prompt(_)
            | Command::Commit { validate: true, .. }
            | Command::Branch {
                name: None,
                delete: None,
                ..
            }
            | Command::Tag(crate::sequence_args::Tag {
                name: None,
                delete: None,
                ..
            })
            | Command::Workspace {
                op: WorkspaceOp::List,
                ..
            }
            | Command::Remote {
                op: RemoteOp::List,
                ..
            }
            | Command::Identity {
                op: IdentityOp::List,
                ..
            }
            | Command::Meta {
                op: MetaOp::Show { .. }
            }
            | Command::Lfs { op: LfsOp::Ls }
    )
}
