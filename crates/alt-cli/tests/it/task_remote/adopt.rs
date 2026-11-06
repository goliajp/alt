use super::*;
use std::{process::Stdio, time::Instant};

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn pause_library(root: &Path) -> std::path::PathBuf {
    let source = root.join("pause-fsync.c");
    let library = root.join("pause-fsync.so");
    std::fs::write(&source,r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static _Atomic int fired=0;
int fsync(int fd) {
    int (*original)(int)=dlsym(RTLD_NEXT,"fsync");
    int result=original(fd);
    const char *directory=getenv("ALT_ADOPT_PAUSE_DIRECTORY");
    const char *present=getenv("ALT_ADOPT_PAUSE_PRESENT");
    const char *absent=getenv("ALT_ADOPT_PAUSE_ABSENT");
    if(result==0 && directory && present && !atomic_load(&fired)) {
        char link[64],path[4096];
        snprintf(link,sizeof(link),"/proc/self/fd/%d",fd);
        ssize_t n=readlink(link,path,sizeof(path)-1);
        if(n>=0) {
            path[n]=0;
            if(!strcmp(path,directory) && access(present,F_OK)==0 && (!absent || access(absent,F_OK)!=0)
                && !atomic_exchange(&fired,1)) raise(SIGSTOP);
        }
    }
    return result;
}
"#).unwrap();
    let result = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-o"])
        .arg(&library)
        .arg(&source)
        .arg("-ldl")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    library
}
fn snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}
fn interrupt_at_boundary(mut command: Command, phase: &str, log: &Path) {
    let mut child = ChildGuard(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state =
            std::fs::read_to_string(format!("/proc/{}/status", child.0.id())).unwrap_or_default();
        if state
            .lines()
            .any(|l| l.starts_with("State:") && l.split_whitespace().nth(1) == Some("T"))
        {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "{phase}: not at durable boundary: {}",
            std::fs::read_to_string(log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
}
fn refusal_with_deadline(work: &Path, token: &str, args: &[&str], log: &Path) -> String {
    let output = std::fs::File::create(log).unwrap();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_alt"))
            .current_dir(work)
            .args(args)
            .env("ALT_NO_DAEMON", "1")
            .env("ALT_SERVER_TOKEN", token)
            .env("USER", "remote-test")
            .env("GIT_AUTHOR_NAME", "remote-test")
            .env("GIT_AUTHOR_EMAIL", "remote@test")
            .env_remove("ALT_PROTECTED_OVERRIDE")
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(!status.success(), "expected refusal");
            return std::fs::read_to_string(log).unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "refusal blocked waiting for the original request"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
#[ignore = "requires Linux x86_64/ext4, isolated PostgreSQL, Git and C compiler; kills only owned CLI processes at durable adoption boundaries"]
fn interrupted_received_task_adoption_can_be_explicitly_reauthorized() {
    let server = Server::new();
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let scratch = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    let alice = root.path().join("alice");
    let bob = root.path().join("bob");
    let aw = root.path().join("alice-work");
    std::fs::create_dir(&alice).unwrap();
    ok(&alice, &server.token, &["init", "."]);
    std::fs::write(alice.join("source.txt"), "base\n").unwrap();
    std::fs::write(alice.join("gate.sh"), "test -s source.txt\n").unwrap();
    ok(&alice, &server.token, &["add", "."]);
    ok(&alice, &server.token, &["commit", "-m", "base"]);
    let base = root.path().join("baseline-git");
    ok(&alice, &server.token, &["export", base.to_str().unwrap()]);
    let url = format!("{}/git/repository", server.url);
    git(
        &base,
        &server.token,
        &[
            "push",
            "--atomic",
            &url,
            "refs/heads/main",
            "refs/notes/alt/meta",
        ],
    );
    let bob_git = root.path().join("bob-git");
    git(
        root.path(),
        &server.bob,
        &["clone", &url, bob_git.to_str().unwrap()],
    );
    git(
        &bob_git,
        &server.bob,
        &["fetch", "origin", "refs/notes/alt/meta:refs/notes/alt/meta"],
    );
    ok(&bob_git, &server.bob, &["import", bob.to_str().unwrap()]);
    ok(
        &alice,
        &server.token,
        &["workspace", "add", "work", aw.to_str().unwrap()],
    );
    let tools = json!([{"name":"bash","path":"/bin/bash","version_args":["--version"]}]);
    let environment = json!({"PATH":"/usr/bin:/bin"});
    let probe = value(
        scratch.path(),
        &server.token,
        &[
            "runner",
            "probe",
            "--bindings",
            &tools.to_string(),
            "--environment",
            &environment.to_string(),
            "--json",
        ],
    );
    let definition = json!({"purpose":"resume an interrupted explicit adoption","authority":"owner","allowed_paths":["source.txt"],"auxiliary":[],"exclusions":[],"validation":{"commands":[["bash","gate.sh"]],"cwd":".","environment":{"values":environment,"complete":true},"tools":probe["tools"],"external":[],"validation":{"outputs":[]}}});
    let task = value(
        &aw,
        &server.token,
        &[
            "task",
            "create",
            "--definition",
            &definition.to_string(),
            "--target",
            "refs/heads/main",
            "--json",
        ],
    );
    let id = string(&task, "task_id");
    connect(&aw, &server, &server.token, "origin");
    let plan = json!({"selection":{"roots":[{"kind":"task","id":id}],"companions":[{"kind":"task-baseline","id":task["baseline_id"]}],"omissions":[],"attestations":[]},"refs":[]});
    let planned = preview(
        &aw,
        &server.token,
        "origin",
        &plan,
        &root.path().join("plan.json"),
    );
    let publication = string(&planned, "request_digest");
    value(
        &aw,
        &server.token,
        &[
            "task",
            "remote",
            "publish",
            string(&planned, "publication_id"),
            "--json",
        ],
    );
    let library = pause_library(root.path());
    for (index, phase) in ["marker", "response"].into_iter().enumerate() {
        let work = root.path().join(phase);
        ok(
            &bob,
            &server.bob,
            &["workspace", "add", phase, work.to_str().unwrap()],
        );
        if index == 0 {
            connect(&work, &server, &server.bob, "origin");
        }
        value(
            &work,
            &server.bob,
            &[
                "task",
                "remote",
                "receive",
                "origin",
                "--kind",
                "task",
                "--id",
                id,
                "--publication",
                publication,
                "--json",
            ],
        );
        let mut old = format!("{:032x}", 100 + index);
        let new = format!("{:032x}", 200 + index);
        let mut request = bob.join(".alt/requests").join(&old);
        let parent = bob
            .join(".alt/workspaces")
            .join(phase)
            .join("task-adoptions");
        let marker = parent.join(format!("{id}.json"));
        let selected = if phase == "marker" {
            parent.clone()
        } else {
            request.clone()
        };
        let present = if phase == "marker" {
            marker.clone()
        } else {
            request.join("response.bin")
        };
        let log = root.path().join(format!("{phase}.log"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
        command
            .current_dir(&work)
            .args([
                "task",
                "remote",
                "adopt",
                id,
                "--publication",
                publication,
                "--request-id",
                &old,
                "--json",
            ])
            .env("ALT_NO_DAEMON", "1")
            .env("ALT_SERVER_TOKEN", &server.bob)
            .env("USER", "remote-test")
            .env("GIT_AUTHOR_NAME", "remote-test")
            .env("GIT_AUTHOR_EMAIL", "remote@test")
            .env_remove("ALT_PROTECTED_OVERRIDE")
            .env("LD_PRELOAD", &library)
            .env("ALT_ADOPT_PAUSE_DIRECTORY", &selected)
            .env("ALT_ADOPT_PAUSE_PRESENT", &present)
            .env("ALT_ADOPT_PAUSE_ABSENT", request.join("completed.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(&log).unwrap()));
        interrupt_at_boundary(command, phase, &log);
        let mut previous = std::fs::read(&marker).unwrap();
        let mut old_files = snapshot(&request);
        assert!(!old_files.contains_key("completed.json"));
        assert_eq!(old_files.contains_key("response.bin"), phase == "response");
        assert!(
            !run(&work, &server.bob, &["candidate", "create", id, "--json"])
                .status
                .success()
        );
        let ordinary = run(
            &work,
            &server.bob,
            &[
                "task",
                "remote",
                "adopt",
                id,
                "--publication",
                publication,
                "--json",
            ],
        );
        assert!(!ordinary.status.success());
        assert!(
            String::from_utf8_lossy(&ordinary.stderr).contains("already has an adoption record")
        );
        println!(
            "CONFIRMED {phase}: actual CLI killed; original pending and ordinary adoption blocked"
        );
        let wrong = format!("{:032x}", 300 + index);
        let arguments = |request: &str| {
            vec![
                "task".to_owned(),
                "remote".to_owned(),
                "adopt".to_owned(),
                id.to_owned(),
                "--publication".to_owned(),
                publication.to_owned(),
                "--reconcile-request".to_owned(),
                request.to_owned(),
                "--json".to_owned(),
            ]
        };
        let wrong_args = arguments(&wrong);
        let wrong_result = refusal_with_deadline(
            &work,
            &server.bob,
            &wrong_args.iter().map(String::as_str).collect::<Vec<_>>(),
            &root.path().join(format!("{phase}-wrong.log")),
        );
        assert!(
            wrong_result.contains("exact original request"),
            "{wrong_result}"
        );
        let active = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(request.join("lock"))
            .unwrap();
        use std::os::fd::AsRawFd;
        // SAFETY: this test owns the live original-request file descriptor.
        assert_eq!(
            unsafe { libc::flock(active.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let active_args = arguments(&old);
        let active_result = refusal_with_deadline(
            &work,
            &server.bob,
            &active_args.iter().map(String::as_str).collect::<Vec<_>>(),
            &root.path().join(format!("{phase}-active.log")),
        );
        assert!(
            active_result.contains("original request is still running"),
            "{active_result}"
        );
        drop(active);
        assert_eq!(std::fs::read(&marker).unwrap(), previous);
        assert_eq!(snapshot(&request), old_files);
        let first_request = request.clone();
        let first_files = old_files.clone();
        let first_marker = previous.clone();
        if phase == "response" {
            let interrupted = format!("{:032x}", 400 + index);
            let next_request = bob.join(".alt/requests").join(&interrupted);
            let next_log = root.path().join("reconciliation-response.log");
            let mut command = Command::new(env!("CARGO_BIN_EXE_alt"));
            command
                .current_dir(&work)
                .args([
                    "task",
                    "remote",
                    "adopt",
                    id,
                    "--publication",
                    publication,
                    "--reconcile-request",
                    &old,
                    "--request-id",
                    &interrupted,
                    "--json",
                ])
                .env("ALT_NO_DAEMON", "1")
                .env("ALT_SERVER_TOKEN", &server.bob)
                .env("USER", "remote-test")
                .env("GIT_AUTHOR_NAME", "remote-test")
                .env("GIT_AUTHOR_EMAIL", "remote@test")
                .env_remove("ALT_PROTECTED_OVERRIDE")
                .env("LD_PRELOAD", &library)
                .env("ALT_ADOPT_PAUSE_DIRECTORY", &next_request)
                .env("ALT_ADOPT_PAUSE_PRESENT", next_request.join("response.bin"))
                .env(
                    "ALT_ADOPT_PAUSE_ABSENT",
                    next_request.join("completed.json"),
                )
                .stdout(Stdio::null())
                .stderr(Stdio::from(std::fs::File::create(&next_log).unwrap()));
            interrupt_at_boundary(command, "reconciliation response", &next_log);
            old = interrupted;
            request = next_request;
            previous = std::fs::read(&marker).unwrap();
            old_files = snapshot(&request);
            assert!(old_files.contains_key("response.bin"));
            assert!(!old_files.contains_key("completed.json"));
            assert!(
                !run(&work, &server.bob, &["candidate", "create", id, "--json"])
                    .status
                    .success()
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&previous).unwrap()["version"],
                2
            );
            println!(
                "CONFIRMED reconciliation: actual second CLI killed before whole-command completion"
            );
        }
        let result = value(
            &work,
            &server.bob,
            &[
                "task",
                "remote",
                "adopt",
                id,
                "--publication",
                publication,
                "--reconcile-request",
                &old,
                "--request-id",
                &new,
                "--json",
            ],
        );
        assert_eq!(result["adopted_task_id"], id);
        assert_eq!(result["supersedes"]["request"], old);
        let digest = blake3::hash(&previous).to_hex().to_string();
        assert_eq!(result["supersedes"]["digest"], digest);
        assert_eq!(
            std::fs::read(parent.join("history").join(format!("{digest}.json"))).unwrap(),
            previous
        );
        assert_eq!(
            snapshot(&request),
            old_files,
            "old intent/response must remain unchanged"
        );
        assert_eq!(snapshot(&first_request), first_files);
        let first_digest = blake3::hash(&first_marker).to_hex().to_string();
        assert_eq!(
            std::fs::read(parent.join("history").join(format!("{first_digest}.json"))).unwrap(),
            first_marker
        );
        let history = parent.join("history").join(format!("{digest}.json"));
        let mut corrupt = previous.clone();
        corrupt.push(b' ');
        std::fs::write(&history, &corrupt).unwrap();
        assert!(
            !run(&work, &server.bob, &["candidate", "create", id, "--json"])
                .status
                .success()
        );
        std::fs::write(&history, &previous).unwrap();
        value(&work, &server.bob, &["candidate", "create", id, "--json"]);
        assert!(
            !run(
                &work,
                &server.bob,
                &[
                    "task",
                    "remote",
                    "adopt",
                    id,
                    "--publication",
                    publication,
                    "--reconcile-request",
                    &new,
                    "--json"
                ]
            )
            .status
            .success()
        );
        // A source record is still the original remote record, not a fabricated
        // successful creation request in this repository.
        let creator = task["task"]["creator_request"].as_str().unwrap();
        assert!(!bob.join(".alt/requests").join(creator).exists());
        println!("PASS {phase}: new explicit authorization with original history retained");
    }
}
