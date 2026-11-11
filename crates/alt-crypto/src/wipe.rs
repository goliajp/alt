//! Clearing secrets from memory in a way the optimizer may not drop.

use std::sync::atomic::{Ordering, compiler_fence};

/// overwrites every element with zero
pub(crate) fn wipe<T: Copy + Default>(secret: &mut [T]) {
    for slot in secret.iter_mut() {
        // a volatile store through a live exclusive reference to a `Copy` value
        unsafe { std::ptr::write_volatile(slot, T::default()) };
    }
    compiler_fence(Ordering::SeqCst);
}
