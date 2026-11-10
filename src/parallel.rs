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

/// Clamp a worker budget to the amount of work: at least 1, at most
/// `units`.
#[inline]
pub(crate) fn effective_workers(budget: usize, units: usize) -> usize {
    budget.min(units).max(1)
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
        for _ in 0..workers {
            let sender = sender.clone();
            let (next, stop, init, work) = (&next, &stop, &init, &work);
            scope.spawn(move || {
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
            });
        }
        drop(sender);
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
}
