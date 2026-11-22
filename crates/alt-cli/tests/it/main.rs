//! Single integration-test binary for alt-cli.
mod abandoned_add;
mod add_clash;
mod add_conflict;
mod add_hidden;
mod add_ignore;
mod add_modes;
mod add_paths;
mod add_renormalize;
mod add_star;
mod add_unmerged;
mod alt_server_push;
mod altd_server;
mod altd_server_added_commits;
mod altd_server_connected;
mod altd_server_landing;
mod altd_server_left_unsynced;
mod altd_server_no_new_objects;
mod altd_server_recorded;
mod altd_server_refused_objects;
mod altd_server_signed_commits;
mod altd_server_trees;
mod altd_server_unsynced_objects;
mod amend;
mod argument_bindings;
mod bisect;
mod blame;
mod branch_switch;
mod byte_names;
mod capability_gate;
mod charset;
mod cherry_pick;
mod cherry_pick_resume;
mod ci_validate;
mod clone;
mod closed_fds;
mod commit_batch;
mod commit_loop;
mod commit_meta;
mod complete;
mod config;
#[cfg(target_os = "linux")]
mod config_paths;
// the points to be killed at are in a debug build only
#[cfg(debug_assertions)]
mod conflict_killed;
mod conflict_reset;
mod conflict_untracked;
mod crash_fixture;
mod credential_helper;
mod cwd_paths;
mod daemon_client;
mod daemon_home;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod daemon_locale;
mod daemon_server;
mod daemon_stop;
mod damage_messages;
mod diff;
mod diff_git;
mod eol;
mod exclude_dirs;
mod exclude_order;
mod fetch;
mod flow;
mod flow_enforce;
mod flow_kill;
mod flow_release_hotfix;
mod fsck;
mod fsck_lost;
mod fsck_reach;
mod fsck_tails;
mod fsck_trees;
mod fsync_by_branch;
mod gitlink;
mod gitlink_said;
mod help_texts;
mod identity;
mod ignore_sources;
#[cfg(target_os = "linux")]
mod import_again;
mod import_materialize;
mod index_blobs;
mod index_link;
mod index_seen;
mod json_outputs;
mod left_snapshots;
mod lfs;
mod lock_order;
mod log_patch;
mod log_tails;
mod machine_results;
mod merge;
mod merge_abort;
mod merge_criss_cross;
mod merge_modes;
mod mixed_versions;
mod monorepo_bench;
mod nested_repo;
mod never_synced_runs;
mod old_daemon;
mod op_log;
mod operation_state;
mod past_damage;
mod path_gate;
mod pathspec_git;
mod patterns;
mod pick_by_commit;
mod power_cut;
mod precommit;
mod prompt;
mod push;
mod push_no_new_objects;
mod push_policy;
mod push_wire_ext;
mod read_json;
mod readers;
mod rebase;
mod recovery_checked;
mod remote;
mod replay_refused;
mod request_operations;
mod revert;
mod scan_cache;
mod scan_daemon;
#[cfg(target_os = "linux")]
mod scan_daemon_inputs;
#[cfg(target_os = "linux")]
mod scan_daemon_mounts;
mod scan_daemon_switch;
#[cfg(target_os = "linux")]
mod scan_daemon_watches;
mod scan_records;
mod shell;
mod show;
mod sink_barrier;
mod staged_durability;
mod staged_stored;
mod status_json;
mod status_labels;
mod status_replaced;
mod status_untracked;
// the points to be killed at are in a debug build only
#[cfg(debug_assertions)]
mod step_changed;
#[cfg(debug_assertions)]
mod step_crash;
#[cfg(debug_assertions)]
mod step_given_up;
mod step_left;
#[cfg(debug_assertions)]
mod step_notes;
#[cfg(debug_assertions)]
mod step_recovered_synced;
mod step_refused;
#[cfg(debug_assertions)]
mod step_stuck;
#[cfg(debug_assertions)]
mod step_stuck_found;
mod step_synced;
mod step_undo;
mod stopped_trees;
mod store_refresh;
mod submission_records;
mod switch_kinds;
mod sync_order;
#[cfg(target_os = "linux")]
mod synced_files;
mod tag;
mod task_delivery;
mod task_evidence;
mod task_remote;
mod task_runner;
mod torn_index;
mod torn_pack;
mod unconfirmed;
mod unconfirmed_tails;
mod undo_add;
mod unsynced_lost;
mod url_rewrite;
mod verify_commit;
mod wire_corpus;
mod wire_test_server;
mod workspace;
mod workspace_branch_guard;
mod write_json;

mod auth;
mod cli_matrix;
mod corpus_cli;
mod corpus_history;
mod filter_process;
mod funcname;
mod funcname_drivers;
mod funcname_git;
mod funcname_long;
mod git_settings;
// the points to be killed at are in a debug build only
#[cfg(debug_assertions)]
mod recover_shared_refs;
mod recovery_kinds;
mod stack_overflow;
mod value_hints;

/// Puts `bytes` at `path` as a writer of state does: in a new file, renamed
/// into place. The file that was there may be kept under another name, as
/// an index is by its recovery snapshot, and is not written through.
pub fn replace_file(path: &std::path::Path, bytes: impl AsRef<[u8]>) {
    let new = path.with_extension("put");
    std::fs::write(&new, bytes).unwrap();
    std::fs::rename(new, path).unwrap();
}

/// The `alt` binary under test, kept from the git configuration of the
/// machine the tests run on, as the git they compare it with is. A test
/// of that configuration gives the command a home of its own.
pub fn alt_command() -> std::process::Command {
    let mut alt = std::process::Command::new(env!("CARGO_BIN_EXE_alt"));
    alt.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    alt
}
