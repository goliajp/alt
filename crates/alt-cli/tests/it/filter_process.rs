//! A filter that stays running (`filter.<name>.process`) converts every
//! file of a command, as it does for git.

use std::path::Path;
use std::process::{Command, Output};

/// A filter process that swaps letters thirteen places along. Its
/// arguments are the file it logs to and what it can do; a path with
/// `refuse` in it gets an error, one with `abort` ends that side.
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
    if 'refuse' in path:
        say('status=error')
        continue
    if 'abort' in path:
        say('status=abort')
        continue
    say('status=success')
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
    let others = ["a.r", "b.r", "k.r", "p.r", "y.r", "z.r"];
    std::fs::write(pair.dst.join("m-abort.r"), "Two\n").unwrap();
    for name in others {
        std::fs::write(pair.dst.join(name), "Hello\n").unwrap();
    }
    std::fs::write(&pair.logs[1], "").unwrap();
    ok(pair.alt(&["add", "."]));
    let log = pair.log(1);
    let gave_up = log.iter().position(|l| l == "clean m-abort.r");
    let gave_up = gave_up.unwrap_or_else(|| panic!("{log:?}"));
    // the process is asked nothing behind the file it gave up on
    assert_eq!(&log[gave_up + 1..], &[] as &[String], "{log:?}");
    ok(pair.alt(&["commit", "-m", "gave up"]));
    assert_eq!(ok(pair.alt(&["show", "HEAD:m-abort.r"])), "Two\n");
    // what it was asked before is stored cleaned, the rest as it is
    let mut left_alone = 0;
    for name in others {
        let cleaned = log[..gave_up].contains(&format!("clean {name}"));
        let stored = ok(pair.alt(&["show", &format!("HEAD:{name}")]));
        let expected = if cleaned { "Uryyb\n" } else { "Hello\n" };
        assert_eq!(stored, expected, "{name}: {log:?}");
        left_alone += usize::from(!cleaned);
    }
    assert!(
        left_alone > 0,
        "no file came behind the one given up on: {log:?}"
    );
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
