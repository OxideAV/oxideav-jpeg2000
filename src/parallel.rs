//! Dependency-free fan-out helpers over `std::thread::scope`.
//!
//! Every helper is **serial by default**: with a budget of one worker
//! (or a single unit of work) nothing is spawned and the closures run
//! on the calling thread in index order. With a larger budget the same
//! closures run on scoped worker threads, and the results are handed
//! back **in index order**, so callers observe the same sequence of
//! results — and the same first error — at every budget.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

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

/// Run `work(&mut state, index)` for every `index` in `0..count` and
/// feed each result to `sink(index, result)` on the calling thread, in
/// increasing index order.
///
/// `init` builds one piece of reusable scratch state per worker (one in
/// total on the serial path). A `sink` error stops the run: no further
/// result is sunk, the workers wind down, and the error is returned.
/// Work on indices past the failing one may already have been
/// performed; it is discarded.
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
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel::<(usize, T)>();
        let mut spawned = 0usize;
        for _ in 0..workers {
            let sender = sender.clone();
            let (next, stop, init, work) = (&next, &stop, &init, &work);
            let worker = move || {
                let mut state = init();
                while !stop.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= count {
                        break;
                    }
                    if sender.send((index, work(&mut state, index))).is_err() {
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
        held.resize_with(count, || None);
        let mut cursor = 0usize;
        for (index, result) in receiver {
            held[index] = Some(result);
            while cursor < count {
                let Some(result) = held[cursor].take() else {
                    break;
                };
                if let Err(error) = sink(cursor, result) {
                    stop.store(true, Ordering::Relaxed);
                    return Err(error);
                }
                cursor += 1;
            }
        }
        Ok(())
    })
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
    let runs = std::sync::Mutex::new(items.chunks_mut(per_run * unit).enumerate());
    let drain = || {
        loop {
            let claimed = match runs.lock() {
                Ok(mut runs) => runs.next(),
                Err(poisoned) => poisoned.into_inner().next(),
            };
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
