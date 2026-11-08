//! `DIRHOP_TRACE=1`: timings of the startup steps, printed to stderr once the picker has
//! closed, so slow machines can be measured instead of guessed at.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Trace {
    start: Instant,
    marks: Mutex<Vec<(u128, String)>>,
}

static TRACE: OnceLock<Option<Trace>> = OnceLock::new();

fn get() -> Option<&'static Trace> {
    TRACE
        .get_or_init(|| {
            std::env::var_os("DIRHOP_TRACE")
                .filter(|v| !v.is_empty() && v != "0")
                .map(|_| Trace { start: Instant::now(), marks: Mutex::new(Vec::new()) })
        })
        .as_ref()
}

pub fn start() {
    get();
}

pub fn mark(what: impl FnOnce() -> String) {
    if let Some(t) = get() {
        let us = t.start.elapsed().as_micros();
        t.marks.lock().unwrap().push((us, what()));
    }
}

pub fn print() {
    let Some(t) = get() else { return };
    for (us, what) in t.marks.lock().unwrap().iter() {
        eprintln!("dirhop trace {:>8.2} ms  {what}", *us as f64 / 1000.0);
    }
}
