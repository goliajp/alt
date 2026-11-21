//! A filter that stays running (`filter.<name>.process`) converts every
//! file of a command, as it does for git.

use std::path::Path;
use std::process::{Command, Output};

/// A filter process that swaps letters thirteen places along. Its
/// arguments are the file it logs to and what it can do; a path with
/// `refuse` in it gets an error, one with `abort` ends that side, and
/// the other words make it break the protocol in one way each.
const FILTER: &str = r#"
import sys
log = open(sys.argv[1], 'a')
can = sys.argv[2:]
inp, out = sys.stdin.buffer, sys.stdout.buffer
def read():
    head = inp.read(4)
    if len(head) < 4:
        sys.exit(0)
    size = int(head, 16)
    return None if size == 0 else inp.read(size - 4)
def lines():
    got = []
    while True:
        packet = read()
        if packet is None:
            return got
        got.append(packet.decode().rstrip('\n'))
def write(payload):
    out.write(b'%04x' % (len(payload) + 4) + payload)
def flush():
    out.write(b'0000')
    out.flush()
def say(*texts):
    for text in texts:
        write(text.encode() + b'\n')
    flush()
once = can[:1] == ['once']
gives_up = can[:1] == ['gives-up']
assert lines() == ['git-filter-client', 'version=2']
say('git-filter-server', 'version=2')
asked = lines()
say(*['capability=' + c for c in can if 'capability=' + c in asked])
log.write('start\n')
log.flush()
table = bytes.maketrans(
    b'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ',
    b'nopqrstuvwxyzabcdefghijklmNOPQRSTUVWXYZABCDEFGHIJKLM')
while True:
    request = dict(line.split('=', 1) for line in lines())
    content = b''
    while True:
        packet = read()
        if packet is None:
            break
        content += packet
    path = request['pathname']
    log.write(request['command'] + ' ' + path + '\n')
    log.flush()
    if gives_up and 'gave up' not in open(sys.argv[1]).read():
        log.write('gave up\n')
        log.flush()
        say('status=abort')
        continue
    if 'refuse' in path:
        say('status=error')
        continue
    if 'abort' in path:
        say('status=abort')
        continue
    if once and 'ended' not in open(sys.argv[1]).read():
        log.write('ended\n')
        log.flush()
        sys.exit(1)
    if 'flood' in path:
        out.write(b'zzzz' + b'x' * 400000)
        out.flush()
    if 'short' in path:
        out.write(b'0003')
        out.flush()
    if 'plus' in path:
        out.write(b'+013status=success\n')
        flush()
    else:
        say('status=success')
    if 'long' in path:
        out.write(b'fff5' + b'x' * 65521)
        flush()
    if 'regret' in path:
        write(b'half')
        flush()
        say('status=error')
        continue
    result = content.translate(table)
    for at in range(0, len(result), 65516):
        write(result[at:at + 65516])
    flush()
    flush()
"#;

fn run(tool: &str, repo: &Path, log: &Path, args: &[&str]) -> Output {
    let program = match tool {
        "git" => "git".into(),
        _ => std::path::PathBuf::from(env!("CARGO_BIN_EXE_alt")),
    };
    Command::new(program)
        .current_dir(repo)
        .env("FILTER_LOG", log)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .args(args)
        .output()
        .unwrap()
}

/// As `run`, for a command that may never end: it is given half a
/// minute.
fn run_within_limit(tool: &str, repo: &Path, log: &Path, args: &[&str]) -> Output {
    use std::process::Stdio;
    let program = match tool {
        "git" => "git".into(),
        _ => std::path::PathBuf::from(env!("CARGO_BIN_EXE_alt")),
    };
    let mut child = Command::new(program)
        .current_dir(repo)
        .env("FILTER_LOG", log)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let begun = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if begun.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{tool} {args:?} did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

struct Pair {
    _dirs: [alt_os::temp::TempDir; 3],
    src: std::path::PathBuf,
    dst: std::path::PathBuf,
    logs: [std::path::PathBuf; 2],
}

impl Pair {
    fn git(&self, args: &[&str]) -> Output {
        run("git", &self.src, &self.logs[0], args)
    }

    fn alt(&self, args: &[&str]) -> Output {
        run("alt", &self.dst, &self.logs[1], args)
    }

    fn log(&self, tool: usize) -> Vec<String> {
        let text = std::fs::read_to_string(&self.logs[tool]).unwrap_or_default();
        text.lines().map(str::to_owned).collect()
    }

    fn tree(&self, tool: &str) -> String {
        let commit = match tool {
            "git" => ok(self.git(&["cat-file", "-p", "HEAD"])),
            _ => ok(self.alt(&["cat-file", "-p", "HEAD"])),
        };
        commit.lines().next().unwrap().to_owned()
    }
}

/// A git repository whose `*.r` files go through the filter process,
/// which can do `can`; committed, checked out afresh, and imported.
fn imported(can: &str, required: bool, files: &[(&str, &str)]) -> Pair {
    let dirs = [0; 3].map(|_| alt_os::temp::tempdir().unwrap());
    let (home, src, dst) = (dirs[0].path(), dirs[1].path(), dirs[2].path());
    std::fs::write(home.join("filter.py"), FILTER).unwrap();
    let pair = Pair {
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
        logs: [home.join("git.log"), home.join("alt.log")],
        _dirs: dirs,
    };
    ok(pair.git(&["init", "-q", "-b", "main", "."]));
    let script = pair.logs[0].with_file_name("filter.py");
    let command = format!("python3 '{}' \"$FILTER_LOG\" {can}", script.display());
    ok(pair.git(&["config", "filter.rot.process", &command]));
    if required {
        ok(pair.git(&["config", "filter.rot.required", "true"]));
    }
    std::fs::write(pair.src.join(".gitattributes"), "*.r filter=rot\n").unwrap();
    for (name, content) in files {
        std::fs::write(pair.src.join(name), content).unwrap();
    }
    ok(pair.git(&["add", "."]));
    ok(pair.git(&["commit", "-qm", "base"]));
    for (name, _) in files {
        std::fs::remove_file(pair.src.join(name)).unwrap();
    }
    ok(pair.git(&["checkout", "-q", "--", "."]));
    let target = pair.dst.to_str().unwrap().to_owned();
    ok(run("alt", &pair.src, &pair.logs[1], &["import", &target]));
    for (name, _) in files {
        let ours = std::fs::read(pair.dst.join(name)).unwrap();
        let theirs = std::fs::read(pair.src.join(name)).unwrap();
        assert_eq!(ours, theirs, "{name} after checkout");
    }
    pair
}

#[test]
fn one_filter_process_converts_every_file_of_a_command() {
    let big = "The quick brown fox.\n".repeat(8000);
    let files = [
        ("a.r", "Hello\n"),
        ("b.r", big.as_str()),
        ("c.txt", "Plain\n"),
    ];
    let pair = imported("clean smudge", true, &files);
    // checked out through the filter: the working tree has the text
    assert_eq!(
        std::fs::read_to_string(pair.dst.join("a.r")).unwrap(),
        "Hello\n"
    );
    let log = pair.log(1);
    assert_eq!(log.iter().filter(|l| *l == "start").count(), 1, "{log:?}");
    assert!(log.contains(&"smudge a.r".to_owned()) && log.contains(&"smudge b.r".to_owned()));
    assert!(!log.iter().any(|l| l.ends_with("c.txt")), "{log:?}");
    let status = ok(pair.alt(&["status"]));
    assert!(status.contains("nothing to commit"), "{status}");

    // stored through the filter: both tools commit the same tree
    for root in [&pair.src, &pair.dst] {
        std::fs::write(root.join("a.r"), "Hello again\n").unwrap();
        std::fs::write(root.join("b.r"), big.replace("fox", "cat")).unwrap();
        std::fs::write(root.join("new.r"), "Fresh\n").unwrap();
    }
    ok(pair.git(&["add", "."]));
    ok(pair.git(&["commit", "-qm", "edit"]));
    std::fs::write(&pair.logs[1], "").unwrap();
    ok(pair.alt(&["add", "."]));
    // scanned and then stored, each file went through the filter once
    let log = pair.log(1);
    assert_eq!(log.iter().filter(|l| *l == "start").count(), 1, "{log:?}");
    for name in ["a.r", "b.r", "new.r"] {
        let cleaned = format!("clean {name}");
        assert_eq!(log.iter().filter(|l| **l == cleaned).count(), 1, "{log:?}");
    }
    ok(pair.alt(&["commit", "-m", "edit"]));
    assert_eq!(pair.tree("alt"), pair.tree("git"));
    let stored = ok(pair.alt(&["show", "HEAD:new.r"]));
    assert_eq!(stored, "Serfu\n");
}

#[test]
fn a_process_that_cannot_or_will_not_filter_a_file_leaves_it_as_it_is() {
    // it only cleans: what is checked out stays as it is stored
    let files = [
        ("a.r", "Hello\n"),
        ("refuse.r", "Kept\n"),
        ("z.r", "Last\n"),
    ];
    let pair = imported("clean", false, &files);
    assert_eq!(
        std::fs::read_to_string(pair.dst.join("a.r")).unwrap(),
        "Uryyb\n"
    );
    // the file it answers an error for was stored unfiltered
    assert_eq!(
        std::fs::read_to_string(pair.dst.join("refuse.r")).unwrap(),
        "Kept\n"
    );
    for root in [&pair.src, &pair.dst] {
        std::fs::write(root.join("a.r"), "One\n").unwrap();
        std::fs::write(root.join("z.r"), "Three\n").unwrap();
    }
    ok(pair.git(&["add", "."]));
    ok(pair.git(&["commit", "-qm", "edit"]));
    ok(pair.alt(&["add", "."]));
    ok(pair.alt(&["commit", "-m", "edit"]));
    assert_eq!(pair.tree("alt"), pair.tree("git"));
    assert_eq!(ok(pair.alt(&["show", "HEAD:a.r"])), "Bar\n");
    assert_eq!(ok(pair.alt(&["show", "HEAD:refuse.r"])), "Kept\n");
}

#[test]
fn a_file_the_process_gives_up_on_is_stored_as_it_is_as_in_git() {
    // the only filtered file: which files come behind it differs
    // between the tools
    let pair = imported("clean", false, &[("c.txt", "Plain\n")]);
    for root in [&pair.src, &pair.dst] {
        std::fs::write(root.join("abort.r"), "Two\n").unwrap();
    }
    ok(pair.git(&["add", "."]));
    ok(pair.alt(&["add", "."]));
    for tool in 0..2 {
        assert!(pair.log(tool).contains(&"clean abort.r".to_owned()));
    }
    ok(pair.git(&["commit", "-qm", "gave up"]));
    ok(pair.alt(&["commit", "-m", "gave up"]));
    assert_eq!(pair.tree("alt"), pair.tree("git"));
    assert_eq!(ok(pair.alt(&["show", "HEAD:abort.r"])), "Two\n");
}

#[test]
fn after_a_process_gives_up_no_other_file_of_the_command_is_cleaned() {
    let pair = imported("clean", false, &[("c.txt", "Plain\n")]);
    // this one gives up at the first file it is asked about, whichever
    let script = pair.logs[0].with_file_name("filter.py");
    let command = format!(
        "python3 '{}' \"$FILTER_LOG\" gives-up clean",
        script.display()
    );
    ok(pair.alt(&["config", "filter.rot.process", &command]));
    let names = ["a.r", "b.r", "k.r", "m.r", "p.r", "y.r", "z.r"];
    for name in names {
        std::fs::write(pair.dst.join(name), "Hello\n").unwrap();
    }
    std::fs::write(&pair.logs[1], "").unwrap();
    ok(pair.alt(&["add", "."]));
    // the process is asked nothing behind the file it gave up on
    let log = pair.log(1);
    assert_eq!(log.len(), 3, "{log:?}");
    assert_eq!((log[0].as_str(), log[2].as_str()), ("start", "gave up"));
    ok(pair.alt(&["commit", "-m", "gave up"]));
    for name in names {
        let stored = ok(pair.alt(&["show", &format!("HEAD:{name}")]));
        assert_eq!(stored, "Hello\n", "{name}: {log:?}");
    }
}

#[test]
fn a_required_filter_process_that_fails_stops_the_command() {
    let pair = imported("clean smudge", true, &[("a.r", "Hello\n")]);
    std::fs::write(pair.dst.join("refuse.r"), "No\n").unwrap();
    std::fs::write(pair.src.join("refuse.r"), "No\n").unwrap();
    assert!(!pair.git(&["add", "."]).status.success());
    let refused = pair.alt(&["add", "."]);
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(message.contains("clean filter 'rot' failed"), "{message}");
}

#[test]
fn a_success_the_process_takes_back_leaves_the_file_as_it_is() {
    let pair = imported("clean", false, &[("c.txt", "Plain\n")]);
    for root in [&pair.src, &pair.dst] {
        std::fs::write(root.join("regret.r"), "Two\n").unwrap();
        std::fs::write(root.join("fine.r"), "Hello\n").unwrap();
    }
    ok(pair.git(&["add", "."]));
    ok(pair.alt(&["add", "."]));
    ok(pair.git(&["commit", "-qm", "taken back"]));
    ok(pair.alt(&["commit", "-m", "taken back"]));
    assert_eq!(pair.tree("alt"), pair.tree("git"));
    assert_eq!(ok(pair.alt(&["show", "HEAD:regret.r"])), "Two\n");
    assert_eq!(ok(pair.alt(&["show", "HEAD:fine.r"])), "Uryyb\n");
}

#[test]
fn a_process_that_ends_is_started_again_for_the_next_file() {
    let pair = imported("clean", false, &[("c.txt", "Plain\n")]);
    // this one ends at the first file it is asked about
    let script = pair.logs[0].with_file_name("filter.py");
    let command = format!("python3 '{}' \"$FILTER_LOG\" once clean", script.display());
    ok(pair.git(&["config", "filter.rot.process", &command]));
    ok(pair.alt(&["config", "filter.rot.process", &command]));
    let names = ["a.r", "b.r", "k.r", "p.r", "y.r", "z.r"];
    for root in [&pair.src, &pair.dst] {
        for name in names {
            std::fs::write(root.join(name), "Hello\n").unwrap();
        }
    }
    for log in &pair.logs {
        std::fs::write(log, "").unwrap();
    }
    ok(pair.git(&["add", "."]));
    ok(pair.alt(&["add", "."]));
    let logs = [pair.log(0), pair.log(1)];
    ok(pair.git(&["commit", "-qm", "ended"]));
    ok(pair.alt(&["commit", "-m", "ended"]));
    // which file comes first differs between the tools; each stores
    // that one as it is and the others cleaned by a second process
    for (tool, show) in [(0, "git"), (1, "alt")] {
        let log = &logs[tool];
        let starts = log.iter().filter(|l| *l == "start").count();
        assert_eq!(starts, 2, "{show}: {log:?}");
        let stored = |name: &str| match tool {
            0 => ok(pair.git(&["show", &format!("HEAD:{name}")])),
            _ => ok(pair.alt(&["show", &format!("HEAD:{name}")])),
        };
        let cleaned = names.iter().filter(|n| stored(n) == "Uryyb\n").count();
        let as_it_is = names.iter().filter(|n| stored(n) == "Hello\n").count();
        assert_eq!((cleaned, as_it_is), (5, 1), "{show}: {log:?}");
    }
}

#[test]
fn a_packet_of_no_allowed_length_ends_the_command_as_in_git() {
    for (name, said) in [
        ("short.r", "protocol error: bad line length 3"),
        ("plus.r", "protocol error: bad line length character: +013"),
        ("long.r", "protocol error: bad line length 65521"),
        ("flood.r", "protocol error: bad line length character: zzzz"),
    ] {
        // not even a filter that may fail is let off
        let pair = imported("clean", false, &[("c.txt", "Plain\n")]);
        for root in [&pair.src, &pair.dst] {
            std::fs::write(root.join(name), "Two\n".repeat(40_000)).unwrap();
        }
        let theirs = run_within_limit("git", &pair.src, &pair.logs[0], &["add", "."]);
        let ours = run_within_limit("alt", &pair.dst, &pair.logs[1], &["add", "."]);
        for (tool, refused) in [("git", theirs), ("alt", ours)] {
            let message = String::from_utf8_lossy(&refused.stderr).into_owned();
            assert!(!refused.status.success(), "{tool} on {name}");
            assert!(message.contains(said), "{tool} on {name}: {message}");
        }
    }
}

#[test]
fn a_process_that_does_not_start_is_reported_for_each_file_and_leaves_them_as_they_are() {
    let pair = imported("clean", false, &[("c.txt", "Plain\n")]);
    ok(pair.git(&["config", "filter.rot.process", "no-such-filter --here"]));
    ok(pair.alt(&["config", "filter.rot.process", "no-such-filter --here"]));
    for root in [&pair.src, &pair.dst] {
        std::fs::write(root.join("a.r"), "One\n").unwrap();
        std::fs::write(root.join("b.r"), "Two\n").unwrap();
    }
    let said = "error: initialization for subprocess 'no-such-filter --here' failed";
    let added = pair.alt(&["add", "."]);
    let message = String::from_utf8_lossy(&added.stderr).into_owned();
    assert!(added.status.success(), "{message}");
    assert_eq!(message.matches(said).count(), 2, "{message}");
    ok(pair.alt(&["commit", "-m", "unfiltered"]));
    // git 2.47 reads the greeting of the process in a way that ends the
    // command when the process is gone, after it reported the process
    // for a file or before; git 2.56 reports it and goes on
    let theirs = pair.git(&["add", "."]);
    let message = String::from_utf8_lossy(&theirs.stderr).into_owned();
    if theirs.status.success() {
        assert_eq!(message.matches(said).count(), 2, "{message}");
        ok(pair.git(&["commit", "-qm", "unfiltered"]));
        assert_eq!(pair.tree("alt"), pair.tree("git"));
    } else {
        let ended = "fatal: the remote end hung up unexpectedly";
        assert!(message.contains(ended), "{message}");
    }
    assert_eq!(ok(pair.alt(&["show", "HEAD:a.r"])), "One\n");
}
