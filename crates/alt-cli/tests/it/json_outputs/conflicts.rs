use super::Scene;

/// `main` and `feat` both rewrite line two of `f.txt`; `feat` also adds a
/// file of its own.
pub fn diverged(name: &'static str) -> Scene {
    let mut s = Scene::new(name);
    s.cd("repo");
    s.setup(&["init", "."]);
    s.write("f.txt", "l1\nl2\nl3\n");
    s.write("other.txt", "o\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "base"]);
    s.setup(&["branch", "apart"]);
    s.setup(&["switch", "-c", "feat"]);
    s.write("f.txt", "l1\nfeat\nl3\n");
    s.write("only-feat.txt", "x\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "feat: change line two"]);
    s.write("later.txt", "later\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "feat: later"]);
    s.setup(&["switch", "main"]);
    s.write("f.txt", "l1\nmain\nl3\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "main: change line two"]);
    s
}

#[test]
fn merge_conflict() {
    let mut s = diverged("merge-conflict");
    s.json(&["merge", "feat", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["diff", "--json"]);
    s.json(&["diff", "--cached", "--json"]);
    s.json(&["commit", "-m", "too early", "--json"]);
    s.json(&["switch", "feat", "--json"]);
    s.json(&["operation", "pending", "--json"]);
    s.write("f.txt", "l1\nboth\nl3\n");
    s.json(&["add", "f.txt", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["commit", "-m", "merge feat", "--json"]);
    s.json(&["show", "--json"]);
    s.json(&["log", "--json", "-n", "1"]);
    s.json(&["op-log", "-n", "2", "--json"]);
    s.finish();
}

#[test]
fn clean_three_way_merge() {
    let mut s = diverged("merge-three-way");
    s.setup(&["switch", "apart"]);
    s.write("apart.txt", "apart\n");
    s.setup(&["add", "."]);
    s.setup(&["commit", "-m", "apart"]);
    s.json(&["merge", "main", "--json"]);
    s.json(&["show", "--json"]);
    s.json(&["undo", "--json"]);
    s.json(&["merge", "absent", "--json"]);
    s.finish();
}

#[test]
fn rebase_stops_and_resumes() {
    let mut s = diverged("rebase");
    s.setup(&["switch", "feat"]);
    s.json(&["rebase", "main", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["rebase", "--continue", "--json"]);
    s.json(&["rebase", "--abort", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["rebase", "main", "--json"]);
    s.write("f.txt", "l1\nrebased\nl3\n");
    s.setup(&["add", "f.txt"]);
    s.json(&["rebase", "--continue", "--json"]);
    s.json(&["log", "--json"]);
    s.json(&["rebase", "main", "--json"]);
    s.json(&["rebase", "--abort", "--json"]);
    s.json(&["undo", "--json"]);
    s.setup(&["switch", "main"]);
    s.setup(&["switch", "-c", "skipping"]);
    s.setup(&["switch", "feat"]);
    s.json(&["rebase", "skipping", "--json"]);
    s.json(&["rebase", "--skip", "--json"]);
    s.json(&["op-log", "-n", "4", "--json"]);
    s.finish();
}

#[test]
fn revert_and_cherry_pick() {
    let mut s = diverged("revert-cherry-pick");
    s.json(&["cherry-pick", "feat", "--json"]);
    s.json(&["show", "--json"]);
    s.json(&["cherry-pick", "feat~1", "--json"]);
    s.json(&["status", "--json"]);
    s.write("f.txt", "l1\npicked\nl3\n");
    s.setup(&["add", "."]);
    s.json(&["commit", "-m", "picked by hand", "--json"]);
    s.json(&["revert", "HEAD~2", "--json"]);
    s.json(&["status", "--json"]);
    s.json(&["revert", "--continue", "--json"]);
    s.json(&["revert", "--abort", "--json"]);
    s.json(&["revert", "--abort", "--json"]);
    s.json(&["revert", "HEAD~2", "--json"]);
    s.write("f.txt", "l1\nreverted by hand\nl3\n");
    s.setup(&["add", "."]);
    s.json(&["revert", "--continue", "--json"]);
    s.json(&["revert", "HEAD", "--json"]);
    s.json(&["revert", "HEAD", "HEAD~1", "--no-edit", "--json"]);
    s.json(&["revert", "absent", "--json"]);
    s.json(&["log", "--json", "-n", "3"]);
    s.finish();
}
