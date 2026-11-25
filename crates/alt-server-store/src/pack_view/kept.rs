//! The answers of earlier walks: in memory, a few of each repository within
//! a bound of bytes, and on disk, the latest of each repository.

use super::stored;
use alt_pack_segment::{Led, Oid};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, Weak};

/// what was found for the tips a repository had at one moment
pub(super) struct Kept {
    /// the repository, as [`Store::led_under`] has it
    pub(super) repository: String,
    /// in the order of the ids, each once
    pub(super) tips: Vec<Oid>,
    pub(super) led: Arc<Led>,
    /// all the tips lead to, not what a walk came to before it ended
    pub(super) whole: bool,
}

/// the answers kept, the latest last, and how many of them of one repository:
/// the tips a request read a moment before another moved them are among them
const KEPT: usize = 16;
const KEPT_EACH: usize = 4;
/// What the answers kept hold in memory at the most, a part two of them share
/// counted once: out of the quarter of the memory of the container that the
/// bounds of the configuration leave to the rest of the process. One answer
/// beyond it is not kept, and a repository whose refs lead to that many
/// objects goes on from what is on disk for it.
const KEPT_BYTES: usize = 128 << 20;

pub(super) static ASKED: Mutex<Vec<Kept>> = Mutex::new(Vec::new());
/// What was found for a repository beyond the bound alone, for as long as a
/// request holds it: those at the same time share it, and the answers kept
/// are not let go of for it.
static BEYOND: LazyLock<Mutex<HashMap<String, (Vec<Oid>, Weak<Led>)>>> =
    LazyLock::new(Default::default);

/// what is held for a repository beyond the bound by a request now
fn beyond(repository: &str) -> Option<(Vec<Oid>, Arc<Led>)> {
    let beyond = BEYOND.lock().expect("beyond");
    let (tips, led) = beyond.get(repository)?;
    Some((tips.clone(), led.upgrade()?))
}
/// how many objects what is on disk for a repository holds, where this process
/// read or wrote it
static STORED: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

/// the directory of the stored bytes and the id of the repository, of what
/// [`Store::led_under`] made
fn of(repository: &str) -> (&Path, &str) {
    let (root, id) = repository.split_once('\0').expect("as led_under has it");
    (Path::new(root), id)
}

/// What is on disk for a repository nothing is kept for in memory, kept as
/// well where it is within the bound.
pub(super) fn loaded(repository: &str) -> Option<(Vec<Oid>, Arc<Led>)> {
    let asked = ASKED.lock().expect("led");
    if asked.iter().any(|kept| kept.repository == repository) {
        return None;
    }
    drop(asked);
    if let Some(held) = beyond(repository) {
        return Some(held);
    }
    let (root, id) = of(repository);
    let (tips, led) = stored::read(root, id)?;
    let mut stored = STORED.lock().expect("stored");
    stored.insert(repository.to_owned(), led.len());
    drop(stored);
    let led = Arc::new(led);
    keep(repository, tips.clone(), led.clone(), true);
    Some((tips, led))
}

/// Writes what a walk found to disk where it was found from nothing, or holds
/// an eighth more than what is there: one who starts again walks no more than
/// that eighth. What is on disk is found anew wherever it is missing, so a
/// write that fails fails nothing else.
pub(super) fn store(repository: &str, tips: &[Oid], led: &Led, from_nothing: bool) {
    let stored = STORED.lock().expect("stored").get(repository).copied();
    if from_nothing || stored.is_none_or(|n| led.len() > n + n / 8) {
        let (root, id) = of(repository);
        if stored::written(root, id, tips, led).is_ok() {
            STORED
                .lock()
                .expect("stored")
                .insert(repository.to_owned(), led.len());
        }
    }
}

/// what is kept for exactly these tips, where it is whole
pub(super) fn kept(repository: &str, tips: &[Oid]) -> Option<Arc<Led>> {
    let asked = ASKED.lock().expect("led");
    let found = (asked.iter())
        .find(|kept| kept.whole && kept.repository == repository && kept.tips == tips);
    let found = found.map(|kept| kept.led.clone());
    drop(asked);
    found.or_else(|| {
        beyond(repository)
            .filter(|(held, _)| held == tips)
            .map(|(_, led)| led)
    })
}

/// Keeps what was found for `tips`, in the place of what was kept for the same
/// tips, and lets go of the oldest beyond what is kept. One that alone is
/// beyond the bound is not kept, and nothing is let go of for it: it is held
/// for the requests that hold it.
pub(super) fn keep(repository: &str, tips: Vec<Oid>, led: Arc<Led>, whole: bool) {
    let kept = Kept {
        repository: repository.to_owned(),
        tips,
        led,
        whole,
    };
    if let Some(alone) = kept_within(&mut ASKED.lock().expect("led"), kept, KEPT_BYTES)
        && alone.whole
    {
        let held = (alone.tips, Arc::downgrade(&alone.led));
        let mut beyond = BEYOND.lock().expect("beyond");
        beyond.retain(|_, (_, led)| led.strong_count() > 0);
        beyond.insert(repository.to_owned(), held);
    }
}

/// [`keep`] among `asked` within `bytes`: the answer back where it alone is
/// beyond them.
fn kept_within(asked: &mut Vec<Kept>, kept: Kept, bytes: usize) -> Option<Kept> {
    if held(std::slice::from_ref(&kept)) > bytes {
        return Some(kept);
    }
    let (repository, tips) = (kept.repository.as_str(), &kept.tips);
    asked.retain(|other| other.repository != repository || other.tips != *tips);
    let of_this = asked.iter().filter(|kept| kept.repository == repository);
    if of_this.count() >= KEPT_EACH {
        let oldest = asked.iter().position(|kept| kept.repository == repository);
        asked.remove(oldest.expect("one of them"));
    }
    if asked.len() >= KEPT {
        asked.remove(0);
    }
    asked.push(kept);
    bounded(asked, bytes);
    None
}

/// Lets go of the oldest answers until those left hold `bytes` at the most:
/// of the one kept last as well, where it is beyond them alone.
fn bounded(asked: &mut Vec<Kept>, bytes: usize) {
    while held(asked) > bytes {
        asked.remove(0);
    }
}

/// the bytes of the answers kept, a part two of them share counted once
fn held(asked: &[Kept]) -> usize {
    let mut parts = HashMap::new();
    for kept in asked {
        parts.extend(kept.led.held());
        parts.insert(kept.tips.as_ptr() as usize, kept.tips.len() * 20);
    }
    parts.values().sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_let_go_of_from_the_oldest_until_what_they_hold_is_within_bound() {
        let objects = |from: u8, n: u8| (from..from + n).map(|byte| [byte; 20]).collect();
        let answer = |tips: Vec<Oid>, led: &Arc<Led>| Kept {
            repository: "one".into(),
            tips,
            led: led.clone(),
            whole: true,
        };
        let ten = Arc::new(Led::again(objects(0, 10)).unwrap());
        let other = Arc::new(Led::again(objects(100, 10)).unwrap());
        // two answers that share what they hold count it once
        let mut asked = vec![
            answer(vec![], &ten),
            answer(vec![], &ten),
            answer(vec![[1; 20]], &other),
        ];
        assert_eq!(held(&asked), 420);
        bounded(&mut asked, 420);
        assert_eq!(asked.len(), 3);
        bounded(&mut asked, 419);
        assert_eq!(held(&asked), 220);
        assert!(Arc::ptr_eq(&asked[0].led, &other));
        bounded(&mut asked, 219);
        assert!(asked.is_empty());
    }

    #[test]
    fn an_answer_beyond_the_bound_alone_is_not_kept_and_lets_go_of_nothing() {
        let objects = |from: u8, n: u8| (from..from + n).map(|byte| [byte; 20]).collect();
        let answer = |repository: &str, led: Vec<Oid>| Kept {
            repository: repository.into(),
            tips: vec![],
            led: Arc::new(Led::again(led).unwrap()),
            whole: true,
        };
        let mut asked = Vec::new();
        assert!(kept_within(&mut asked, answer("one", objects(0, 5)), 300).is_none());
        assert!(kept_within(&mut asked, answer("two", objects(10, 5)), 300).is_none());
        assert_eq!(held(&asked), 200);
        let beyond = kept_within(&mut asked, answer("three", objects(20, 16)), 300);
        assert_eq!(beyond.map(|kept| kept.repository), Some("three".to_owned()));
        let left: Vec<&str> = asked.iter().map(|kept| kept.repository.as_str()).collect();
        assert_eq!(left, ["one", "two"]);
        // one within the bound lets the oldest go as before
        assert!(kept_within(&mut asked, answer("four", objects(40, 10)), 300).is_none());
        let left: Vec<&str> = asked.iter().map(|kept| kept.repository.as_str()).collect();
        assert_eq!(left, ["two", "four"]);
    }
}
