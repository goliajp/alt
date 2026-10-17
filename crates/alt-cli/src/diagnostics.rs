//! Per-command diagnostics without writing around the result encoder.

use std::cell::RefCell;
use std::io::Write;
use std::marker::PhantomData;
use std::rc::Rc;

struct Buffer {
    json: bool,
    bytes: Vec<u8>,
}

thread_local! {
    static CURRENT: RefCell<Option<Buffer>> = const { RefCell::new(None) };
}

/// Thread-bound because daemon commands run on independent worker threads.
pub struct Capture {
    previous: Option<Buffer>,
    _thread: PhantomData<Rc<()>>,
}

impl Capture {
    pub fn start(json: bool) -> Self {
        let previous = CURRENT.with(|current| {
            current.replace(Some(Buffer {
                json,
                bytes: Vec::new(),
            }))
        });
        Self {
            previous,
            _thread: PhantomData,
        }
    }

    pub fn finish(self) -> Vec<u8> {
        CURRENT.with(|current| {
            current
                .borrow_mut()
                .take()
                .map(|buffer| buffer.bytes)
                .unwrap_or_default()
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

pub fn json_mode() -> bool {
    CURRENT.with(|current| current.borrow().as_ref().is_some_and(|buffer| buffer.json))
}

pub fn emit(bytes: &[u8]) -> std::io::Result<()> {
    let captured = CURRENT.with(|current| {
        if let Some(buffer) = current.borrow_mut().as_mut() {
            buffer.bytes.extend_from_slice(bytes);
            true
        } else {
            false
        }
    });
    if !captured {
        std::io::stderr().lock().write_all(bytes)?;
    }
    Ok(())
}
