//! Single integration-test binary for alt-cli.
mod add_ignore;
mod add_paths;
mod altd_server;
mod amend;
mod argument_bindings;
mod bisect;
mod blame;
mod branch_switch;
mod capability_gate;
mod charset;
mod cherry_pick;
mod ci_validate;
mod clone;
mod commit_loop;
mod commit_meta;
mod config;
mod credential_helper;
mod daemon_client;
mod daemon_server;
mod diff;
mod diff_git;
mod eol;
mod fetch;
mod flow;
mod flow_enforce;
mod flow_kill;
mod flow_release_hotfix;
mod identity;
mod import_materialize;
mod json_outputs;
mod lfs;
mod log_patch;
mod machine_results;
mod merge;
mod merge_criss_cross;
mod monorepo_bench;
mod nested_repo;
mod op_log;
mod patterns;
mod precommit;
mod push;
mod push_policy;
mod push_wire_ext;
mod read_json;
mod rebase;
mod remote;
mod revert;
mod show;
mod status_json;
mod switch_kinds;
mod tag;
mod task_delivery;
mod task_evidence;
mod task_remote;
mod task_runner;
mod undo_add;
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
mod git_settings;
mod recovery_kinds;

/// The `alt` binary under test, kept from the git configuration of the
/// machine the tests run on, as the git they compare it with is. A test
/// of that configuration gives the command a home of its own.
pub fn alt_command() -> std::process::Command {
    let mut alt = std::process::Command::new(env!("CARGO_BIN_EXE_alt"));
    alt.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    alt
}
