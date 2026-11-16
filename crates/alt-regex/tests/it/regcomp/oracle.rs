//! The C library asked on a thread of its own, since for some patterns
//! it never answers.

use alt_regex::Options;

use super::{Theirs, in_utf8_locale};

type Spans = Vec<Option<(usize, usize)>>;
pub type Question = (Vec<u8>, Options, Vec<Vec<u8>>);
/// For each text its spans; `None` for a pattern that is refused.
pub type Answer = Option<Vec<Option<Spans>>>;

/// The C library on a thread of its own. For some patterns it never
/// answers; it is then left behind and another takes its place.
pub struct Oracle {
    ask: std::sync::mpsc::Sender<Question>,
    answer: std::sync::mpsc::Receiver<Answer>,
    utf8: bool,
}

impl Oracle {
    pub fn start(utf8: bool) -> Self {
        let (ask, asked) = std::sync::mpsc::channel::<Question>();
        let (answered, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if utf8 {
                in_utf8_locale();
            }
            for (pattern, options, texts) in asked {
                let theirs = Theirs::new(&pattern, options);
                let compiled = theirs.0.is_some();
                let reply = compiled.then(|| texts.iter().map(|t| theirs.spans(t)).collect());
                if answered.send(reply).is_err() {
                    break;
                }
            }
        });
        Oracle { ask, answer, utf8 }
    }

    /// What the C library says, unless it takes longer than a pattern
    /// of this size ever should.
    pub fn ask(&mut self, question: Question) -> Option<Answer> {
        self.ask
            .send(question)
            .expect("the thread waits for questions");
        let patience = std::time::Duration::from_secs(20);
        let reply = self.answer.recv_timeout(patience).ok();
        if reply.is_none() {
            *self = Oracle::start(self.utf8);
        }
        reply
    }
}
