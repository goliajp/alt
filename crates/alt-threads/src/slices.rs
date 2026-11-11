//! Work over the elements of a slice, split in halves down to runs of `grain`
//! elements, which one thread handles in order.

use crate::join;
use std::{
    mem::{ManuallyDrop, MaybeUninit},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// Calls `work` on every element. `grain` is how many elements are too few to be
/// worth handing to another thread: one for elements that each take long, more
/// the cheaper they are.
pub fn for_each<T: Sync>(items: &[T], grain: usize, work: impl Fn(&T) + Sync) {
    each(items, grain.max(1), &work);
}

fn each<T: Sync, F: Fn(&T) + Sync>(items: &[T], grain: usize, work: &F) {
    if items.len() <= grain {
        items.iter().for_each(work);
        return;
    }
    let (left, right) = items.split_at(items.len() / 2);
    join(|| each(left, grain, work), || each(right, grain, work));
}

/// The results of `work` on every element, in the order of the elements.
pub fn map<T: Sync, R: Send>(items: &[T], grain: usize, work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let mut out = uninit(items.len());
    fill(items, &mut out, grain.max(1), &work);
    // `fill` wrote every element
    unsafe { init(out) }
}

fn fill<T: Sync, R: Send, F: Fn(&T) -> R + Sync>(
    items: &[T],
    out: &mut [MaybeUninit<R>],
    grain: usize,
    work: &F,
) {
    if items.len() <= grain {
        for (item, slot) in items.iter().zip(out) {
            slot.write(work(item));
        }
        return;
    }
    let middle = items.len() / 2;
    let (left, right) = items.split_at(middle);
    let (left_out, right_out) = out.split_at_mut(middle);
    join(
        || fill(left, left_out, grain, work),
        || fill(right, right_out, grain, work),
    );
}

/// `map` that hands each element to `work` by value.
pub fn map_owned<T: Send, R: Send>(
    items: Vec<T>,
    grain: usize,
    work: impl Fn(T) -> R + Sync,
) -> Vec<R> {
    // the elements are moved out one by one; the vector only frees its buffer
    let mut items = ManuallyDrop::new(items);
    let (start, len, capacity) = (items.as_mut_ptr(), items.len(), items.capacity());
    // same buffer, seen as places that may have been moved out of
    let mut moved = unsafe { Vec::from_raw_parts(start.cast::<MaybeUninit<T>>(), len, capacity) };
    let mut out = uninit(len);
    take(&mut moved, &mut out, grain.max(1), &work);
    // `take` wrote every element
    unsafe { init(out) }
}

fn take<T: Send, R: Send, F: Fn(T) -> R + Sync>(
    items: &mut [MaybeUninit<T>],
    out: &mut [MaybeUninit<R>],
    grain: usize,
    work: &F,
) {
    if items.len() <= grain {
        for (item, slot) in items.iter_mut().zip(out) {
            // each place is read once: the halves do not overlap
            slot.write(work(unsafe { item.assume_init_read() }));
        }
        return;
    }
    let middle = items.len() / 2;
    let (left, right) = items.split_at_mut(middle);
    let (left_out, right_out) = out.split_at_mut(middle);
    join(
        || take(left, left_out, grain, work),
        || take(right, right_out, grain, work),
    );
}

/// Calls `work` on the elements until one fails, and returns that error. Elements
/// not yet looked at when one fails are skipped; of several failures any one is
/// returned.
pub fn try_for_each<T: Sync, E: Send>(
    items: &[T],
    grain: usize,
    work: impl Fn(&T) -> Result<(), E> + Sync,
) -> Result<(), E> {
    let (failed, error) = (AtomicBool::new(false), Mutex::new(None));
    attempt(items, grain.max(1), &work, &failed, &error);
    error.into_inner().unwrap().map_or(Ok(()), Err)
}

fn attempt<T: Sync, E: Send, F: Fn(&T) -> Result<(), E> + Sync>(
    items: &[T],
    grain: usize,
    work: &F,
    failed: &AtomicBool,
    error: &Mutex<Option<E>>,
) {
    if failed.load(Ordering::Relaxed) {
        return;
    }
    if items.len() <= grain {
        if let Err(found) = items.iter().try_for_each(work) {
            failed.store(true, Ordering::Relaxed);
            *error.lock().unwrap() = Some(found);
        }
        return;
    }
    let (left, right) = items.split_at(items.len() / 2);
    join(
        || attempt(left, grain, work, failed, error),
        || attempt(right, grain, work, failed, error),
    );
}

/// Whether `test` holds for some element. Elements not yet looked at when one is
/// found are skipped.
pub fn any<T: Sync>(items: &[T], grain: usize, test: impl Fn(&T) -> bool + Sync) -> bool {
    let found = AtomicBool::new(false);
    search(items, grain.max(1), &test, &found);
    found.into_inner()
}

fn search<T: Sync, F: Fn(&T) -> bool + Sync>(
    items: &[T],
    grain: usize,
    test: &F,
    found: &AtomicBool,
) {
    if found.load(Ordering::Relaxed) {
        return;
    }
    if items.len() <= grain {
        if items.iter().any(test) {
            found.store(true, Ordering::Relaxed);
        }
        return;
    }
    let (left, right) = items.split_at(items.len() / 2);
    join(
        || search(left, grain, test, found),
        || search(right, grain, test, found),
    );
}

fn uninit<R>(len: usize) -> Vec<MaybeUninit<R>> {
    let mut out = Vec::with_capacity(len);
    // uninitialized places need no initialization
    unsafe { out.set_len(len) };
    out
}

/// The caller has written every element of `out`.
unsafe fn init<R>(out: Vec<MaybeUninit<R>>) -> Vec<R> {
    let mut out = ManuallyDrop::new(out);
    // the same buffer, with the element type it now really holds
    unsafe { Vec::from_raw_parts(out.as_mut_ptr().cast::<R>(), out.len(), out.capacity()) }
}
