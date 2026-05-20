//! Request-owned accounting propagated explicitly across execution threads.
use std::{cell::RefCell, io, sync::Arc};

pub trait BudgetPort: Send + Sync {
    /// Reserve durably before dispatch. A crashed reservation is never refunded.
    fn before_model_invocation(&self) -> io::Result<()>;
    /// Reserve optional work only when the main turn still has capacity.
    fn before_optional_invocation(&self) -> io::Result<()> {
        Err(io::Error::other("Optional model calls unavailable"))
    }
    /// Must be nonblocking: controls may be checked while host storage is locked.
    fn check(&self) -> io::Result<()> {
        Ok(())
    }
    fn suspend(&self) -> io::Result<()> {
        Ok(())
    }
    fn resume(&self) -> io::Result<()> {
        Ok(())
    }
    fn finish(&self) -> io::Result<()> {
        Ok(())
    }
    /// Called after each completed provider call with the usage the provider
    /// reported. Usage is a metric: it never charges or refunds the budget.
    fn record_usage(&self, _usage: &crate::execution::ModelUsage) -> io::Result<()> {
        Ok(())
    }
}
pub type BudgetHandle = Arc<dyn BudgetPort>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct BudgetExceeded {
    pub message: String,
}
impl BudgetExceeded {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
pub fn is_exceeded(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<BudgetExceeded>())
}

thread_local! { static CURRENT: RefCell<Option<BudgetHandle>> = RefCell::new(None); }
pub fn current() -> Option<BudgetHandle> {
    CURRENT.with(|slot| slot.borrow().clone())
}
pub fn charge() -> io::Result<()> {
    current().map_or(Ok(()), |budget| budget.before_model_invocation())
}
pub fn check() -> io::Result<()> {
    current().map_or(Ok(()), |budget| budget.check())
}
pub fn record_usage(usage: &crate::execution::ModelUsage) -> io::Result<()> {
    current().map_or(Ok(()), |budget| budget.record_usage(usage))
}
/// Exclude a human-input wait from the request's active execution clock.
pub fn waiting<T>(work: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    struct Resume(Option<BudgetHandle>);
    impl Drop for Resume {
        fn drop(&mut self) {
            if let Some(handle) = self.0.take() {
                let _ = handle.resume();
            }
        }
    }
    let handle = current();
    if let Some(handle) = &handle {
        handle.suspend()?;
    }
    let mut guard = Resume(handle);
    let result = work();
    if let Some(handle) = guard.0.take() {
        handle.resume()?;
    }
    result
}
pub fn scoped<T>(handle: BudgetHandle, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<BudgetHandle>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CURRENT.with(|slot| slot.replace(Some(handle))));
    work()
}
pub fn inherited<T>(handle: Option<BudgetHandle>, work: impl FnOnce() -> T) -> T {
    match handle {
        Some(handle) => scoped(handle, work),
        None => work(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Count(AtomicUsize);
    impl BudgetPort for Count {
        fn before_model_invocation(&self) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    #[test]
    fn nested_scopes_restore_and_threads_require_explicit_inheritance() {
        let outer = Arc::new(Count(AtomicUsize::new(0)));
        let inner = Arc::new(Count(AtomicUsize::new(0)));
        scoped(outer.clone(), || {
            charge().unwrap();
            scoped(inner.clone(), || charge().unwrap());
            let handle = current();
            std::thread::spawn(move || inherited(handle, || charge().unwrap()))
                .join()
                .unwrap();
            charge().unwrap();
        });
        assert!(current().is_none());
        assert_eq!(outer.0.load(Ordering::SeqCst), 3);
        assert_eq!(inner.0.load(Ordering::SeqCst), 1);
    }
}
