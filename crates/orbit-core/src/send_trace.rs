//! Opt-in timings for one synchronous send on the UI storage worker.
//! Scope by thread so concurrent turn saves are never attributed to this send.
use std::{
    cell::RefCell,
    io::{self, Write},
    time::{Duration, Instant},
};

struct Timings {
    phase: &'static str,
    since: Instant,
    elapsed: Vec<(&'static str, Duration)>,
}

impl Timings {
    fn finish_phase(&mut self, now: Instant) {
        self.elapsed.push((self.phase, now - self.since));
        self.since = now;
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Timings>> = const { RefCell::new(None) };
}

pub fn queued() -> Option<Instant> {
    std::env::var_os("ORBIT_TRACE_SEND").map(|_| Instant::now())
}

pub fn run<T, E>(queued: Option<Instant>, submit: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let Some(start) = queued else { return submit() };
    let now = Instant::now();
    ACTIVE.set(Some(Timings {
        phase: "workspace check",
        since: now,
        elapsed: vec![("storage queue", now - start)],
    }));
    let result = submit();
    let mut times = ACTIVE.take().expect("active send trace");
    let now = Instant::now();
    times.finish_phase(now);
    // Emit after releasing DB/lifecycle guards, including on a failed acceptance.
    let status = if result.is_ok() { "ok" } else { "error" };
    let _ = writeln!(
        io::stderr().lock(),
        "orbit send ({status}, total {:?}): {:?}",
        now - start,
        times.elapsed
    );
    result
}

pub(crate) fn phase(name: &'static str) {
    ACTIVE.with_borrow_mut(|active| {
        if let Some(times) = active {
            times.finish_phase(Instant::now());
            times.phase = name;
        }
    });
}
