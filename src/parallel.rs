//! Dependency-free fan-out helpers over `std::thread::scope`.
//!
//! Every helper is **serial by default**: with a budget of one worker
//! (or a single unit of work) nothing is spawned and the closures run
//! on the calling thread in index order. With a larger budget the same
//! closures run on scoped worker threads, and the results are handed
//! back **in index order**, so callers observe the same sequence of
//! results — and the same first error — at every budget.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, mpsc};

/// The most workers any fan-out uses, whatever the budget: a budget is
/// a cap handed in by the caller, not a request to create that many
/// threads.
const MAX_WORKERS: usize = 256;

/// Clamp a worker budget to the amount of work: at least 1, at most
/// `units` (and [`MAX_WORKERS`]).
#[inline]
pub(crate) fn effective_workers(budget: usize, units: usize) -> usize {
    budget.min(units).clamp(1, MAX_WORKERS)
}

/// The share of work a caller wants per worker before fanning out:
/// `min_per_worker` in normal builds. Fuzzing builds fan out whenever
/// the budget allows, so that the small inputs a fuzzer favours still
/// exercise the threaded paths against the serial ones.
///
/// (`fuzzing` is the configuration flag fuzzing harnesses build the
/// crate under; it is not a declared feature, hence the lint
/// exemption.)
#[inline]
#[allow(unexpected_cfgs)]
pub(crate) fn worker_share(min_per_worker: usize) -> usize {
    if cfg!(fuzzing) { 1 } else { min_per_worker }
}

/// Run `work(&mut state, index)` for every `index` in `0..count` and
/// feed each result to `sink(index, result)` on the calling thread, in
/// increasing index order.
///
/// `init` builds one piece of reusable scratch state per worker (one in
/// total on the serial path). A `sink` error stops the run: no further
/// result is sunk, the workers wind down, and the error is returned.
/// Work on indices past the failing one may already have been
/// performed; it is discarded.
///
/// Workers never run more than a small window ahead of the sink, so
/// the results waiting for their turn stay bounded however uneven the
/// work is.
pub(crate) fn ordered_map<S, T, E, I, W, K>(
    budget: usize,
    count: usize,
    init: I,
    work: W,
    mut sink: K,
) -> Result<(), E>
where
    T: Send,
    I: Fn() -> S + Sync,
    W: Fn(&mut S, usize) -> T + Sync,
    K: FnMut(usize, T) -> Result<(), E>,
{
    let workers = effective_workers(budget, count);
    if workers == 1 {
        let mut state = init();
        for index in 0..count {
            sink(index, work(&mut state, index))?;
        }
        return Ok(());
    }
    // How far past the sink's cursor a worker may start an index.
    let window = 2 * workers;
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    // The sink's cursor (indices below it have been sunk), which the
    // workers wait on when they get too far ahead.
    let sunk = Mutex::new(0usize);
    let advanced = Condvar::new();
    let release_workers = || {
        stop.store(true, Ordering::Relaxed);
        // Taking the lock orders this wake-up after any worker's check
        // of `stop`, so none can miss it.
        drop(lock(&sunk));
        advanced.notify_all();
    };
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel::<(usize, T)>();
        let mut spawned = 0usize;
        for _ in 0..workers {
            let sender = sender.clone();
            let (next, stop, init, work) = (&next, &stop, &init, &work);
            let (sunk, advanced) = (&sunk, &advanced);
            let worker = move || {
                // Should `work` panic, wake the workers waiting on the
                // window so the scope can unwind instead of hanging.
                let _wake = WakeOnPanic {
                    stop,
                    sunk,
                    advanced,
                };
                let mut state = init();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= count {
                        break;
                    }
                    {
                        let mut cursor = lock(sunk);
                        while index >= *cursor + window && !stop.load(Ordering::Relaxed) {
                            cursor = match advanced.wait(cursor) {
                                Ok(guard) => guard,
                                Err(poisoned) => poisoned.into_inner(),
                            };
                        }
                    }
                    if stop.load(Ordering::Relaxed)
                        || sender.send((index, work(&mut state, index))).is_err()
                    {
                        break;
                    }
                }
            };
            // A thread the host refuses to create is simply one worker
            // fewer: the others drain the same index counter.
            if std::thread::Builder::new()
                .spawn_scoped(scope, worker)
                .is_ok()
            {
                spawned += 1;
            }
        }
        drop(sender);
        if spawned == 0 {
            // No worker at all: do the work here, in order.
            let mut state = init();
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    return Ok(());
                }
                sink(index, work(&mut state, index))?;
            }
        }
        // Results arrive in completion order; hold the early ones back
        // until every lower index has been sunk.
        let mut held: Vec<Option<T>> = Vec::new();
        held.resize_with(window + 1, || None);
        let mut cursor = 0usize;
        for (index, result) in receiver {
            // `index < cursor + window` by the workers' wait above.
            held[index % (window + 1)] = Some(result);
            let before = cursor;
            while cursor < count {
                let Some(result) = held[cursor % (window + 1)].take() else {
                    break;
                };
                if let Err(error) = sink(cursor, result) {
                    release_workers();
                    return Err(error);
                }
                cursor += 1;
            }
            if cursor != before {
                *lock(&sunk) = cursor;
                advanced.notify_all();
            }
        }
        Ok(())
    })
}

/// Releases every worker of an [`ordered_map`] run when the thread
/// holding it unwinds.
struct WakeOnPanic<'a> {
    stop: &'a AtomicBool,
    sunk: &'a Mutex<usize>,
    advanced: &'a Condvar,
}

impl Drop for WakeOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.stop.store(true, Ordering::Relaxed);
            drop(lock(self.sunk));
            self.advanced.notify_all();
        }
    }
}

/// Lock a mutex whose protected value stays meaningful even if a
/// holder panicked.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Run `work(index)` for every `index` in `0..count` and collect the
/// results in index order.
pub(crate) fn collect_ordered<T, W>(budget: usize, count: usize, work: W) -> Vec<T>
where
    T: Send,
    W: Fn(usize) -> T + Sync,
{
    let mut out = Vec::with_capacity(count);
    let outcome: Result<(), std::convert::Infallible> = ordered_map(
        budget,
        count,
        || (),
        |(), index| work(index),
        |_, result| {
            out.push(result);
            Ok(())
        },
    );
    match outcome {
        Ok(()) => out,
        Err(never) => match never {},
    }
}

/// Split `items` into at most `budget` contiguous runs of whole
/// `unit`-sized groups and run `work(first_group_index, run)` on each.
/// The calling thread works through the runs alongside up to
/// `budget − 1` spawned workers; with a budget of one (or a single
/// group) the whole slice is one run processed on the calling thread.
pub(crate) fn for_each_run<T, W>(budget: usize, items: &mut [T], unit: usize, work: W)
where
    T: Send,
    W: Fn(usize, &mut [T]) + Sync,
{
    if unit == 0 || items.is_empty() {
        return;
    }
    let groups = items.len().div_ceil(unit);
    let workers = effective_workers(budget, groups);
    if workers == 1 {
        work(0, items);
        return;
    }
    let per_run = groups.div_ceil(workers);
    // Every worker — this thread included — takes the next unclaimed
    // run until none is left, so a thread the host refuses to create
    // costs speed, never coverage.
    let runs = Mutex::new(items.chunks_mut(per_run * unit).enumerate());
    let drain = || {
        loop {
            let claimed = lock(&runs).next();
            let Some((run, chunk)) = claimed else {
                break;
            };
            work(run * per_run, chunk);
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..workers {
            let _ = std::thread::Builder::new().spawn_scoped(scope, drain);
        }
        drain();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_map_sinks_in_index_order_at_every_budget() {
        for budget in [0, 1, 2, 3, 8, 64] {
            let mut seen = Vec::new();
            let outcome: Result<(), ()> = ordered_map(
                budget,
                100,
                || 7usize,
                |state, index| {
                    // Uneven work so completion order differs from
                    // index order on the threaded path.
                    let mut acc = *state;
                    for step in 0..(100 - index) * 500 {
                        acc = acc.wrapping_mul(31).wrapping_add(step);
                    }
                    std::hint::black_box(acc);
                    index * 2
                },
                |index, value| {
                    seen.push((index, value));
                    Ok(())
                },
            );
            assert_eq!(outcome, Ok(()));
            let expected: Vec<_> = (0..100).map(|i| (i, i * 2)).collect();
            assert_eq!(seen, expected, "budget {budget}");
        }
    }

    #[test]
    fn ordered_map_reports_the_first_failure_in_index_order() {
        for budget in [1, 4, 16] {
            let mut sunk = 0usize;
            let outcome = ordered_map(
                budget,
                50,
                || (),
                |(), index| index,
                |index, value| {
                    if value == 17 || value == 30 {
                        return Err(value);
                    }
                    assert_eq!(index, sunk);
                    sunk += 1;
                    Ok(())
                },
            );
            assert_eq!(outcome, Err(17), "budget {budget}");
            assert_eq!(sunk, 17);
        }
    }

    #[test]
    fn ordered_map_keeps_workers_within_the_window() {
        // Index 0 is slow: without a window every other result would
        // pile up behind it. Count how many are in flight at once.
        let budget = 4;
        let in_flight = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let mut expected = 0usize;
        let outcome: Result<(), ()> = ordered_map(
            budget,
            400,
            || (),
            |(), index| {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                if index == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                index
            },
            |index, value| {
                assert_eq!((index, value), (expected, expected));
                expected += 1;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(outcome, Ok(()));
        assert_eq!(expected, 400);
        // Started-but-unsunk results never exceed the window (2 × the
        // workers) plus the workers' own claims.
        assert!(peak.load(Ordering::SeqCst) <= 3 * budget + 1);
    }

    #[test]
    fn ordered_map_propagates_a_worker_panic_without_hanging() {
        let outcome = std::panic::catch_unwind(|| {
            let result: Result<(), ()> = ordered_map(
                4,
                200,
                || (),
                |(), index| {
                    assert!(index != 3, "boom");
                    index
                },
                |_, _| Ok(()),
            );
            result
        });
        assert!(outcome.is_err());
    }

    #[test]
    fn ordered_map_handles_empty_and_single_inputs() {
        for count in [0usize, 1] {
            let collected = collect_ordered(8, count, |index| index + 1);
            assert_eq!(collected, (1..=count).collect::<Vec<_>>());
        }
    }

    #[test]
    fn for_each_run_covers_every_group_once() {
        for budget in [1, 2, 5, 32] {
            for unit in [1usize, 3, 7] {
                let mut items = vec![0u32; 10 * unit + unit / 2];
                for_each_run(budget, &mut items, unit, |first, run| {
                    for (offset, group) in run.chunks_mut(unit).enumerate() {
                        for item in group {
                            *item += (first + offset) as u32 + 1;
                        }
                    }
                });
                for (group, chunk) in items.chunks(unit).enumerate() {
                    assert!(chunk.iter().all(|&v| v == group as u32 + 1));
                }
            }
        }
    }
}
