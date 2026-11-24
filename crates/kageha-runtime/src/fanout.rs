//! Bounded, work-conserving tool dispatch. Adapters own permissions and durable receipts.
use crate::execution::*;
use crate::lifecycle::worker_slots::{current_slot, with_slot};
use std::{io, sync::mpsc};

/// Serialize conflicting resources and global barriers in model order, while
/// admitting independent calls as soon as a slot is free. Results retain input
/// order. Cancellation stops admission and drains already admitted calls; this
/// cannot preempt a non-cooperative adapter or undo dispatched side effects.
pub fn execute_parallel(
    tools: &(dyn ToolPort + Sync),
    calls: &[ToolCall],
    control: &Control<'_>,
    max_parallel: usize,
) -> io::Result<Vec<ToolReply>> {
    if !(1..=16).contains(&max_parallel) || calls.len() > 16 {
        return Err(io::Error::other("Invalid tool fan-out limit"));
    }
    let mut ids = std::collections::HashSet::new();
    let access = calls
        .iter()
        .map(|call| {
            call.validate()?;
            if !ids.insert(&call.id) {
                return Err(io::Error::other("Duplicate tool call identity"));
            }
            control.check()?;
            tools.dispatch_access(call)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let conflicts = |a: usize, b: usize| match (&access[a], &access[b]) {
        (DispatchAccess::Resource(a), DispatchAccess::Resource(b)) => !a.is_empty() && a == b,
        _ => true,
    };
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let mut admitted = vec![false; calls.len()];
        let mut done = vec![false; calls.len()];
        let mut results = vec![None; calls.len()];
        let mut running = 0;
        let mut completed = 0;
        let mut stopped = None;
        loop {
            if stopped.is_none() {
                stopped = control.check().err();
            }
            if stopped.is_none() {
                for index in 0..calls.len() {
                    if running == max_parallel {
                        break;
                    }
                    if admitted[index]
                        || (0..index).any(|earlier| !done[earlier] && conflicts(earlier, index))
                    {
                        continue;
                    }
                    if let Err(error) = control.check() {
                        stopped = Some(error);
                        break;
                    }
                    let tx = tx.clone();
                    let call = &calls[index];
                    let budget = kageha_contracts::request_budget::current();
                    // An exclusive tool owns the whole batch while the run's
                    // worker waits for it. It may lend that worker's slot.
                    let slot = match &access[index] {
                        DispatchAccess::Exclusive => current_slot(),
                        DispatchAccess::Resource(_) => None,
                    };
                    // Builder returns an admission error instead of panicking on OS thread exhaustion.
                    match std::thread::Builder::new()
                        .name("kageha-tool".into())
                        .spawn_scoped(scope, move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| with_slot(slot, || {
                                    kageha_contracts::request_budget::inherited(budget, || {
                                        control.check()?;
                                        tools.execute(call, control)
                                    })
                                })));
                            let panicked = result.is_err();
                            let result = result.unwrap_or_else(|_| {
                                Err(io::Error::other(kageha_contracts::ServiceError::new(
                                    kageha_contracts::ErrorCode::Uncertain,
                                    "Tool executor panicked; inspect its receipt before retrying",
                                )))
                            });
                            let _ = tx.send((index, result, panicked));
                        }) {
                        Ok(_) => {
                            admitted[index] = true;
                            running += 1;
                        }
                        Err(error) => {
                            stopped = Some(error);
                            break;
                        }
                    }
                }
            }
            if running == 0 {
                if let Some(error) = stopped {
                    return Err(error);
                }
                if completed == calls.len() {
                    break;
                }
                return Err(io::Error::other("Tool scheduler made no progress"));
            }
            let (index, reply, panicked) = rx.recv().map_err(io::Error::other)?;
            running -= 1;
            completed += 1;
            done[index] = true;
            if panicked {
                stopped.get_or_insert_with(|| {
                    io::Error::other(kageha_contracts::ServiceError::new(
                        kageha_contracts::ErrorCode::Uncertain,
                        "Tool executor panicked; inspect its receipt before retrying",
                    ))
                });
            }
            match reply {
                Err(error) if kageha_contracts::policy::is_stop(&error) || kageha_contracts::execution::is_uncertain(&error) || kageha_contracts::request_budget::is_exceeded(&error) => {
                    stopped.get_or_insert(error);
                }
                other => {
                    let reply = other.unwrap_or_else(ToolReply::failure);
                    if let Some(error) = reply.terminal_error() { stopped.get_or_insert(error); }
                    results[index] = Some(reply);
                }
            }
        }
        control.check()?;
        Ok(results.into_iter().map(Option::unwrap).collect())
    })
}
