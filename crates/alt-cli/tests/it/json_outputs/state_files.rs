//! Scenes that end with an operation still open, so the state a command
//! stored for a later one is on disk when the scene is kept.

use super::conflicts::diverged;
use super::member;
use super::tasks::{definition, prepared, profile};

#[test]
fn rebase_left_stopped() {
    let mut s = diverged("state-rebase-stopped");
    s.setup(&["switch", "feat"]);
    s.json(&["rebase", "main", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["operation", "pending", "--json"]);
    s.json(&["op-log", "--json"]);
    s.finish();
}

#[test]
fn revert_left_stopped() {
    let mut s = diverged("state-revert-stopped");
    s.write("f.txt", "l1\nagain\nl3\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "main: again"]);
    s.json(&["revert", "HEAD~1", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["operation", "pending", "--json"]);
    s.finish();
}

#[test]
fn task_left_before_review() {
    let (mut s, tools) = prepared("state-task-before-review", "printf verified\n");
    let task = member(
        &s.json(&[
            "task",
            "create",
            "--definition",
            &definition(&tools, "left open"),
            "--target",
            "refs/heads/main",
            "--json",
        ]),
        "task_id",
    );
    s.write("source.txt", "reviewed\n");
    s.setup(&["add", "source.txt"]);
    let candidate = member(
        &s.json(&["candidate", "create", &task, "--json"]),
        "candidate_id",
    );
    let profile = profile(&s);
    s.json(&[
        "evidence",
        "run",
        &candidate,
        "--profile",
        &profile,
        "--json",
    ]);
    s.json(&["request", "pending", "--json"]);
    s.finish();
}
