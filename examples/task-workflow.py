#!/usr/bin/env python3
"""Run a small reviewed task in a new directory, retaining every command receipt."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--alt", type=Path, required=True)
    parser.add_argument("--git", type=Path, required=True)
    parser.add_argument("--directory", type=Path, required=True, help="new directory on APFS or ext4, outside repositories")
    args = parser.parse_args()
    binary, git = args.alt.resolve(strict=True), args.git.resolve(strict=True)
    root = args.directory.expanduser().resolve()
    for parent in [root, *root.parents]:
        if any((parent / marker).exists() or (parent / marker).is_symlink()
               for marker in [".git", ".alt"]):
            parser.error("the example directory must be outside repositories")
    root.mkdir()
    repo, workspace, scratch, output = [root / name for name in ["main", "task", "runs", "git-exit"]]
    scratch.mkdir()
    environment = dict(os.environ, ALT_NO_DAEMON="1", GIT_AUTHOR_NAME="Internal package example",
                       GIT_AUTHOR_EMAIL="example@localhost", GIT_COMMITTER_NAME="Internal package example",
                       GIT_COMMITTER_EMAIL="example@localhost", GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
    log = root / "commands.jsonl"

    def run(cwd, argv, expected=0):
        result = subprocess.run([str(part) for part in argv], cwd=cwd, env=environment,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        with log.open("a") as dst:
            dst.write(json.dumps({"cwd": str(cwd), "argv": [str(x) for x in argv], "exit_code": result.returncode,
                                  "stdout_hex": result.stdout.hex(), "stderr_hex": result.stderr.hex()}) + "\n")
        if result.returncode != expected:
            raise RuntimeError(f"{argv}: expected {expected}, got {result.returncode}; see {log}")
        return result

    def alt(cwd, *argv):
        return json.loads(run(cwd, [binary, *argv, "--json"]).stdout)

    run(root, [binary, "init", repo])
    (repo / "message.txt").write_text("base\n")
    (repo / "gate.sh").write_text('test "$(cat message.txt)" = reviewed\nprintf "verified\\n"\n')
    run(repo, [binary, "add", "message.txt", "gate.sh"])
    run(repo, [binary, "commit", "-m", "chore: create example baseline"])
    run(repo, [binary, "workspace", "add", "task", workspace])
    bindings = [{"name": "bash", "path": "/bin/bash", "version_args": ["--version"]}]
    validation_env = {"PATH": "/usr/bin:/bin"}
    probe = alt(scratch, "runner", "probe", "--bindings", json.dumps(bindings),
                "--environment", json.dumps(validation_env))
    definition = {"purpose": "Demonstrate reviewed delivery", "authority": "explicit example invocation",
                  "allowed_paths": ["message.txt"], "auxiliary": [], "exclusions": [],
                  "validation": {"commands": [["bash", "gate.sh"]], "cwd": ".",
                  "environment": {"values": validation_env, "complete": True}, "tools": probe["tools"],
                  "external": [], "validation": {"outputs": []}}}
    task = alt(workspace, "task", "create", "--definition", json.dumps(definition), "--target", "refs/heads/main")
    (workspace / "message.txt").write_text("reviewed\n")
    run(workspace, [binary, "add", "message.txt"])
    candidate = alt(workspace, "candidate", "create", task["task_id"])
    profile = {"kind": "local", "scratch": str(scratch), "tools": bindings, "timeout_ms": 10000}
    evidence = alt(workspace, "evidence", "run", candidate["candidate_id"], "--profile", json.dumps(profile))
    if not evidence["validity"]["eligible"]:
        raise RuntimeError("example validation did not produce eligible observed evidence")
    changes = alt(workspace, "task", "changes", candidate["candidate_id"])
    delta = changes["diff"]["changes"]
    if len(delta) != 1 or delta[0]["before"]["utf8"] != "base\n" or delta[0]["after"]["utf8"] != "reviewed\n":
        raise RuntimeError("example change differs from the explicitly authorized edit")
    review = alt(workspace, "task", "review", candidate["candidate_id"], "--evidence", evidence["evidence_id"],
                 "--reviewer", "example-invoker", "--reason", "Checked the exact example edit and observed gate",
                 "-m", "docs: demonstrate reviewed delivery")
    run(repo, [binary, "switch", "-c", "holding"])
    index = (repo / ".alt/index").read_bytes()
    request = "d0700000000000000000000000000001"
    integration = alt(workspace, "--request-id", request, "task", "integrate", review["review_id"])
    replay = alt(workspace, "--request-id", request, "task", "integrate", review["review_id"])
    if integration != replay or (repo / ".alt/index").read_bytes() != index:
        raise RuntimeError("integration replay or other workspace preservation failed")
    delivery = alt(workspace, "task", "export", integration["integration_id"], output, "--git", git)
    alt(workspace, "task", "delivery-collect", delivery["delivery_id"], "--git", git)
    run(output, [git, "fsck", "--strict"])
    if run(output, [git, "status", "--porcelain"]).stdout or (output / "message.txt").read_text() != "reviewed\n":
        raise RuntimeError("Git exit is not clean or has the wrong content")
    if (output / ".alt").exists() or (repo / "message.txt").read_text() != "base\n":
        raise RuntimeError("private state leaked or the other workspace changed")
    result = {"task": task["task_id"], "candidate": candidate["candidate_id"], "evidence": evidence["evidence_id"],
              "review": review["review_id"], "integration": integration["integration_id"], "delivery": delivery["delivery_id"],
              "git_exit": str(output), "commands": str(log), "alt_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "git_version": run(root, [git, "--version"]).stdout.decode().strip(), "complete": True}
    (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
