//! The C library asked in a process of its own, since for some patterns
//! it never answers: a process can be stopped, a thread cannot.

use std::ffi::c_uint;
use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use alt_regex::{Options, Syntax};

use super::{GROUPS, Theirs, in_utf8_locale};

type Spans = Vec<Option<(usize, usize)>>;
pub type Question = (Vec<u8>, Options, Vec<Vec<u8>>);
/// For each text its spans; `None` for a pattern that is refused.
pub type Answer = Option<Vec<Option<Spans>>>;

/// Set for the process that answers, to the kind of text it reads.
const SERVES: &str = "ALT_REGEX_ORACLE";
/// What the answers begin behind, past what the test harness prints.
const BEGUN: &[u8] = b"\x00answers\x00";
/// How long the C library has for one pattern and its texts.
const PATIENCE: Duration = Duration::from_secs(1);
const UNSET: u32 = u32::MAX;

unsafe extern "C" {
    fn alarm(seconds: c_uint) -> c_uint;
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
}

fn word(from: &mut impl Read) -> Option<u32> {
    let mut word = [0; 4];
    from.read_exact(&mut word).ok()?;
    Some(u32::from_le_bytes(word))
}

fn bytes(from: &mut impl Read) -> Option<Vec<u8>> {
    let mut bytes = vec![0; word(from)? as usize];
    from.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

fn question(from: &mut impl Read, utf8: bool) -> Option<Question> {
    let flags = word(from)?;
    let options = Options {
        syntax: match flags & 1 {
            0 => Syntax::Extended,
            _ => Syntax::Basic,
        },
        ignore_case: flags & 2 != 0,
        newline: flags & 4 != 0,
        utf8,
    };
    let pattern = bytes(from)?;
    let texts = (0..word(from)?)
        .map(|_| bytes(from))
        .collect::<Option<_>>()?;
    Some((pattern, options, texts))
}

/// `None` where the process ended before it had answered.
fn told(from: &mut impl Read) -> Option<Answer> {
    if word(from)? == 0 {
        return Some(None);
    }
    let mut texts = Vec::new();
    for _ in 0..word(from)? {
        if word(from)? == 0 {
            texts.push(None);
            continue;
        }
        let mut spans = Vec::new();
        for _ in 0..GROUPS {
            let (start, end) = (word(from)?, word(from)?);
            spans.push((start != UNSET).then_some((start as usize, end as usize)));
        }
        texts.push(Some(spans));
    }
    Some(Some(texts))
}

/// Run as the process an [`Oracle`] starts, this answers the questions
/// on its input until that ends; run as a test it has nothing to do.
#[test]
fn the_c_library_answers_in_a_process_of_its_own() {
    let Ok(kind) = std::env::var(SERVES) else {
        return;
    };
    let utf8 = kind == "utf8";
    if utf8 {
        in_utf8_locale();
    }
    let mut asked = BufReader::new(std::io::stdin().lock());
    let mut answered = std::io::stdout().lock();
    answered.write_all(BEGUN).unwrap();
    answered.flush().unwrap();
    while let Some((pattern, options, texts)) = question(&mut asked, utf8) {
        // whoever asked may be gone by the time it is known that the
        // C library never answers: this process then ends by itself
        // SAFETY: a signal that is not caught ends the process
        unsafe { alarm(3 * PATIENCE.as_secs() as c_uint) };
        let theirs = Theirs::new(&pattern, options);
        let mut reply = Vec::new();
        reply.extend(u32::from(theirs.0.is_some()).to_le_bytes());
        if theirs.0.is_some() {
            reply.extend((texts.len() as u32).to_le_bytes());
            for text in &texts {
                let spans = theirs.spans(text);
                reply.extend(u32::from(spans.is_some()).to_le_bytes());
                for span in spans.into_iter().flatten() {
                    let (start, end) = span.map_or((UNSET, UNSET), |s| (s.0 as u32, s.1 as u32));
                    reply.extend(start.to_le_bytes());
                    reply.extend(end.to_le_bytes());
                }
            }
        }
        answered.write_all(&reply).unwrap();
        answered.flush().unwrap();
        // SAFETY: as above; none is waited for any more
        unsafe { alarm(0) };
    }
}

/// The C library in a process of its own. Where it does not answer in
/// time, or ends, the process is stopped and another takes its place.
pub struct Oracle {
    process: Child,
    ask: ChildStdin,
    answer: Receiver<Answer>,
    utf8: bool,
}

impl Oracle {
    pub fn start(utf8: bool) -> Self {
        let this = std::env::current_exe().expect("the path of this test");
        let serve = concat!(
            module_path!(),
            "::the_c_library_answers_in_a_process_of_its_own"
        );
        // the name of a test begins behind that of the crate
        let serve = serve.split_once("::").expect("a test in a module").1;
        let mut process = Command::new(this)
            .args(["--exact", serve, "--nocapture", "--test-threads", "1"])
            .env(SERVES, if utf8 { "utf8" } else { "bytes" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("this test run once more");
        let ask = process.stdin.take().expect("a pipe to it");
        let mut from = BufReader::new(process.stdout.take().expect("a pipe from it"));
        let (answered, answer) = channel();
        std::thread::spawn(move || {
            let (mut seen, mut byte) = (0, [0]);
            while seen < BEGUN.len() {
                if from.read_exact(&mut byte).is_err() {
                    return;
                }
                seen = match byte[0] == BEGUN[seen] {
                    true => seen + 1,
                    false => usize::from(byte[0] == BEGUN[0]),
                };
            }
            while let Some(reply) = told(&mut from) {
                if answered.send(reply).is_err() {
                    break;
                }
            }
        });
        Oracle {
            process,
            ask,
            answer,
            utf8,
        }
    }

    /// What the C library says, unless it takes longer than a pattern
    /// of this size ever should or ends without an answer.
    pub fn ask(&mut self, (pattern, options, texts): Question) -> Option<Answer> {
        let mut asked = Vec::new();
        let basic = u32::from(options.syntax == Syntax::Basic);
        let flags = basic | u32::from(options.ignore_case) << 1 | u32::from(options.newline) << 2;
        asked.extend(flags.to_le_bytes());
        put(&mut asked, &pattern);
        asked.extend((texts.len() as u32).to_le_bytes());
        texts.iter().for_each(|text| put(&mut asked, text));
        let sent = self.ask.write_all(&asked).and_then(|()| self.ask.flush());
        let reply = match sent {
            Ok(()) => self.answer.recv_timeout(PATIENCE).ok(),
            Err(_) => None,
        };
        if reply.is_none() {
            *self = Oracle::start(self.utf8);
        }
        reply
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        // it may be in a search that never ends
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
