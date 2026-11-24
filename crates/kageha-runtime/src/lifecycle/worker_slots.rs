use super::Supervisor;
use std::{
    cell::RefCell,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
};

#[derive(Default)]
struct Depth {
    active: usize,
    parked: usize,
    resuming: usize,
    threads: usize,
}

#[derive(Default)]
pub(super) struct WorkerSlots {
    depths: Mutex<[Depth; 4]>,
    available: Condvar,
}

impl WorkerSlots {
    pub(super) fn set_base_threads(&self, count: usize) {
        for depth in self.depths.lock().unwrap().iter_mut() {
            depth.threads = count;
        }
    }
}

pub(crate) struct Slot {
    supervisor: Arc<Supervisor>,
    depth: u32,
    parked: AtomicBool,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Slot>>> = const { RefCell::new(None) };
}

pub(crate) fn current_slot() -> Option<Arc<Slot>> {
    CURRENT.with(|current| current.borrow().clone())
}

pub(crate) fn with_slot<T>(slot: Option<Arc<Slot>>, execute: impl FnOnce() -> T) -> T {
    struct Reset(Option<Arc<Slot>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CURRENT.with(|current| current.replace(self.0.take()));
        }
    }
    let _reset = Reset(CURRENT.with(|current| current.replace(slot)));
    execute()
}

impl Slot {
    pub(super) fn acquire(supervisor: &Arc<Supervisor>, depth: u32) -> Option<Arc<Self>> {
        let mut depths = supervisor.slots.depths.lock().unwrap();
        let lane = &mut depths[depth as usize];
        if lane.active >= supervisor.limits.per_depth || lane.resuming > 0 {
            return None;
        }
        lane.active += 1;
        Some(Arc::new(Self {
            supervisor: supervisor.clone(),
            depth,
            parked: AtomicBool::new(false),
        }))
    }

    fn park(&self) -> io::Result<Parked<'_>> {
        let supervisor = &self.supervisor;
        let start_worker = {
            let mut depths = supervisor.slots.depths.lock().unwrap();
            let lane = &mut depths[self.depth as usize];
            let start_worker = lane.threads < supervisor.limits.per_depth + lane.parked + 1;
            if start_worker && lane.threads >= supervisor.limits.queue_per_depth {
                return Err(io::Error::other(
                    "Too many runs waiting in this execution queue",
                ));
            }
            if supervisor.stopping.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Runtime is draining",
                ));
            }
            lane.active -= 1;
            lane.parked += 1;
            lane.threads += usize::from(start_worker);
            self.parked.store(true, Ordering::Relaxed);
            start_worker
        };

        let guard = Parked(self);
        supervisor.slots.available.notify_all();
        supervisor.notify();
        if start_worker {
            if let Err(error) = supervisor.spawn_worker(self.depth) {
                supervisor.slots.depths.lock().unwrap()[self.depth as usize].threads -= 1;
                return Err(error);
            }
        }
        Ok(guard)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.supervisor.slots.depths.lock().unwrap()[self.depth as usize].active -= 1;
        self.supervisor.slots.available.notify_all();
    }
}

struct Parked<'a>(&'a Slot);
impl Drop for Parked<'_> {
    fn drop(&mut self) {
        let slot = self.0;
        let supervisor = &slot.supervisor;
        let depth = slot.depth as usize;
        let mut depths = supervisor.slots.depths.lock().unwrap();
        depths[depth].resuming += 1;
        let mut depths = supervisor
            .slots
            .available
            .wait_while(depths, |depths| {
                depths[depth].active >= supervisor.limits.per_depth
            })
            .unwrap();
        let lane = &mut depths[depth];
        lane.resuming -= 1;
        lane.parked -= 1;
        lane.active += 1;
        slot.parked.store(false, Ordering::Relaxed);
    }
}

/// Keep the leased run and its pending decision, but lend its execution slot
/// until the owner responds. The resumed run reacquires a slot before continuing.
pub fn while_waiting<T>(wait: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let slot = current_slot();
    let _parked = match slot.as_deref() {
        Some(slot) if !slot.parked.load(Ordering::Relaxed) => Some(slot.park()?),
        _ => None,
    };
    wait()
}
