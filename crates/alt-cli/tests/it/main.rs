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
mod altd_server_certified;
mod altd_server_connected;
#[cfg(debug_assertions)]
mod altd_server_fetch_failed;
mod altd_server_fetch_time;
mod altd_server_index_memory;
mod altd_server_keepalive;
mod altd_server_landing;
mod altd_server_left_unsynced;
mod altd_server_many_repos;
mod altd_server_no_new_objects;
mod altd_server_pack_limits;
mod altd_server_push_cost;
mod altd_server_push_keepalive;
mod altd_server_push_refused;
mod altd_server_reads;
mod altd_server_recorded;
mod altd_server_refused_objects;
mod altd_server_relay;
mod altd_server_sending;
mod altd_server_shallow;
mod altd_server_shallow_lines;
mod altd_server_signed_commits;
mod altd_server_signed_in;
mod altd_server_slow_peers;
mod altd_server_slow_readers;
mod altd_server_temp;
mod altd_server_thin;
mod altd_server_told;
mod altd_server_trees;
mod altd_server_unsynced_objects;
mod altd_server_untrusted;
mod altd_server_users;
mod altd_server_versions;
mod altd_server_wants;
mod ambiguous_args;
mod amend;
mod amend_empty;
mod argument_bindings;
mod bisect;
mod blame;
mod branch_moved;
mod branch_start;
mod branch_switch;
mod byte_names;
mod capability_gate;
mod casefold;
mod casefold_asked;
mod casefold_composed;
mod casefold_dir_spelled;
mod casefold_dirs;
mod casefold_left;
mod casefold_meet;
mod casefold_undo;
mod casefold_untracked;
mod casefold_wider;
mod charset;
mod checkout_guess;
mod checkout_order;
mod cherry_pick;
mod cherry_pick_resume;
mod ci_validate;
mod clone;
mod closed_fds;
mod commit_author;
mod commit_batch;
mod commit_ids;
mod commit_loop;
mod commit_meta;
mod complete;
mod config;
mod config_layers;
#[cfg(target_os = "linux")]
mod config_paths;
mod config_written;
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
mod diff_between;
mod diff_git;
mod edit_order;
mod eol;
mod exclude_dirs;
mod exclude_order;
mod export_under_way;
#[cfg(target_os = "linux")]
mod failed_sync;
#[cfg(target_os = "linux")]
mod failed_sync_put_aside;
mod fetch;
mod file_mode;
mod flow;
mod flow_enforce;
mod flow_kill;
mod flow_release_hotfix;
mod flow_tags;
mod fsck;
mod fsck_lost;
mod fsck_reach;
mod fsck_tails;
mod fsck_trees;
mod fsync_by_branch;
mod gitlink;
mod gitlink_said;
mod held_branches;
mod help_texts;
mod identity;
mod ignore_sources;
#[cfg(target_os = "linux")]
mod import_again;
mod import_config;
mod import_materialize;
mod import_met;
mod index_blobs;
mod index_link;
mod index_seen;
mod invalid_paths;
mod json_outputs;
mod left_snapshots;
mod lfs;
mod like_git;
mod lock_order;
mod log_patch;
mod log_range;
mod log_tails;
mod machine_results;
mod merge;
mod merge_abort;
mod merge_criss_cross;
mod merge_ff;
mod merge_modes;
mod merge_named;
mod mixed_versions;
mod monorepo_bench;
mod mv;
mod mv_rename;
mod nested_repo;
mod never_synced_runs;
mod of_late;
mod old_daemon;
mod op_log;
mod operation_state;
mod pack_repeated;
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
mod rebase_merges;
mod rebase_merges_base;
mod recovery_checked;
mod reflog;
mod reflog_kept;
mod remote;
mod replay_refused;
mod request_operations;
mod reset;
mod reset_soft;
mod reset_unborn;
mod restore;
mod rev_parse;
mod revert;
mod rewrite_authors;
mod rm;
mod safe_directory;
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
mod signing;
mod sink_barrier;
mod staged_durability;
mod staged_stored;
mod stash;
mod stash_concurrent;
mod stash_matrix;
mod stash_paths;
mod stash_roundtrip;
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
#[cfg(debug_assertions)]
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
mod switch_carry;
mod switch_kinds;
mod switch_picking;
mod sync_order;
#[cfg(target_os = "linux")]
mod synced_files;
mod tag;
mod task_delivery;
mod task_evidence;
mod task_remote;
mod task_runner;
mod task_unnamed;
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
#[cfg(target_os = "macos")]
mod precompose;
// the points to be killed at are in a debug build only
#[cfg(debug_assertions)]
mod recover_shared_refs;
mod recovery_kinds;
mod sequence_entries;
mod sequence_resumed;
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

/// Whom the commits of a test are by where it names nobody else, and
/// the zone they are made in, which is not that of the machine the tests
/// run on.
pub const TEST_SIGNING: [(&str, &str); 5] = [
    ("GIT_AUTHOR_NAME", "Test Author"),
    ("GIT_AUTHOR_EMAIL", "author@test.invalid"),
    ("GIT_COMMITTER_NAME", "Test Committer"),
    ("GIT_COMMITTER_EMAIL", "committer@test.invalid"),
    ("TZ", "UTC"),
];

/// Somebody for the commits a test makes in its own process.
pub fn test_identity() -> alt_cli::native::Identity {
    let named = TEST_SIGNING.map(|(key, value)| (key.to_owned(), value.to_owned()));
    alt_cli::native::Identity::from_map(&named)
}

/// The `alt` binary under test, kept from the git configuration of the
/// machine the tests run on, as the git they compare it with is. A test
/// of that configuration gives the command a home of its own.
pub fn alt_command() -> std::process::Command {
    let mut alt = std::process::Command::new(env!("CARGO_BIN_EXE_alt"));
    alt.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .envs(TEST_SIGNING);
    alt
}
