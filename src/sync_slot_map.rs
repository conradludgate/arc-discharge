use std::sync::{
    atomic::{self, AtomicUsize},
    Mutex, MutexGuard,
};

pub(crate) struct SyncSlotMap {
    exclusive: Mutex<SyncSlotMapExclusive>,
    shared: SyncSlotMapShared,
}

impl SyncSlotMap {
    pub(crate) fn new(cap: usize) -> Self {
        SyncSlotMap {
            exclusive: Mutex::new(SyncSlotMapExclusive { tail: cap }),
            shared: SyncSlotMapShared {
                head: AtomicUsize::new(cap),
                slots: (0..=cap).map(|_| AtomicUsize::new(cap + 1)).collect(),
            },
        }
    }

    pub(crate) fn push(&self, index: usize) {
        self.shared.push(index)
    }
    pub(crate) fn try_lock(&self) -> Option<SyncSlotMapLock<'_>> {
        Some(SyncSlotMapLock {
            exclusive: self.exclusive.try_lock().ok()?,
            shared: &self.shared,
        })
    }
}

pub(crate) struct SyncSlotMapLock<'a> {
    exclusive: MutexGuard<'a, SyncSlotMapExclusive>,
    shared: &'a SyncSlotMapShared,
}

struct SyncSlotMapExclusive {
    tail: usize,
}

struct SyncSlotMapShared {
    head: AtomicUsize,
    slots: Box<[AtomicUsize]>,
}

impl SyncSlotMapShared {
    fn push(&self, index: usize) {
        self.slots[index].store(self.slots.len(), atomic::Ordering::Relaxed);
        let prev = self.head.swap(index, atomic::Ordering::AcqRel);
        self.slots[prev].store(index, atomic::Ordering::Release);
    }
}

pub(crate) enum ReadySlot<T> {
    Ready(T),
    Inconsistent,
    None,
}

impl SyncSlotMapLock<'_> {
    /// The pop function from the 1024cores intrusive MPSC queue algorithm
    pub(crate) fn pop(&mut self) -> ReadySlot<usize> {
        let cap = self.shared.slots.len() - 1;

        let mut tail = self.exclusive.tail;
        let mut next = self.shared.slots[tail].load(atomic::Ordering::Acquire);

        if tail == cap {
            if next > cap {
                return ReadySlot::None;
            }

            self.exclusive.tail = next;
            tail = next;
            next = self.shared.slots[next].load(atomic::Ordering::Acquire);
        }

        if next <= cap {
            self.exclusive.tail = next;
            debug_assert!(tail != cap);
            return ReadySlot::Ready(tail);
        }

        if self.shared.head.load(atomic::Ordering::Acquire) != tail {
            return ReadySlot::Inconsistent;
        }

        self.shared.push(cap);

        next = self.shared.slots[tail].load(atomic::Ordering::Acquire);

        if next <= cap {
            self.exclusive.tail = next;
            return ReadySlot::Ready(tail);
        }

        ReadySlot::Inconsistent
    }
}
