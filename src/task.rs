use std::{
    mem::offset_of,
    pin::Pin,
    ptr::{addr_of, NonNull},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    task::{Context, Waker},
};

use futures_util::{task::AtomicWaker, Future};
use parking_lot::Mutex;

use crate::{
    join_handle::{JoinError, JoinInner},
    linked_list::{FatLink, FatLinked},
    sync_slot_map::ReadySlot,
    MTRuntime, WorkerThreadWaker, HANDLE,
};

const IDLE: u8 = 0;
const RUNNING: u8 = 1;
const COMPLETED: u8 = 2;
const CANCELLED: u8 = 3;
// A notification was consumed by another worker during the current poll.
const NOTIFIED: u8 = 4;

/// An allocated task never moves after its first poll. Only `allocate` creates
/// tasks outside this module, and returns an erased Arc with no downcast/mutable
/// access. Keep fields private: completion/cancellation drop the future in place.
pub(crate) struct Task<F: Future> {
    /// intrusive pointers
    link: FatLink<dyn DynTask>,

    /// pointer to the runtime handle
    handle: Arc<MTRuntime>,

    /// who should be notified of this task completing
    state: AtomicU8,
    waker: AtomicWaker,

    /// the future for this task
    fut: Mutex<JoinInner<F>>,
}

/// # Safety
/// A returned link must belong to this object, remain at a stable address while
/// linked, and carry a callback recovering this same object and concrete type.
/// Scheduling/polling must preserve any pinning guarantees made to the future.
pub(crate) unsafe trait DynTask: Send + Sync + 'static {
    fn schedule_global(self: Arc<Self>);
    fn run(self: Arc<Self>);

    fn register(&self, waker: &Waker);
    fn cancel(self: Arc<Self>);

    fn take_output(&self, output: &mut dyn std::any::Any);

    /// # Safety
    /// `self` must be a valid pointer to a live task.
    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<dyn DynTask>>;
}

impl<F: Future + Send + 'static> Task<F>
where
    F::Output: Send + 'static,
{
    fn schedule_global(this: &Arc<Self>) {
        {
            let mut queue = this.handle.global_queue.lock().unwrap();
            if !this.link.is_linked() {
                queue.push_back(this.clone());
            }
        }

        // if we push to the global queue, we should try wake up a worker to process it
        // if it's already locked, then we know another worker is already being woken up.
        if let Some(mut parked) = this.handle.parked_workers.try_lock() {
            loop {
                if let ReadySlot::Ready(index) = parked.pop() {
                    let worker = &this
                        .handle
                        .workers
                        .get()
                        .expect("runtime not initialised properly")[index];

                    let mut x = worker.parker.lock().unwrap();
                    match *x {
                        Some(WorkerThreadWaker::Parked) => {
                            *x = None;
                            worker.unparker.notify_one();
                            break;
                        }
                        None => {
                            continue;
                        }
                    }
                } else {
                    this.handle.io_handle.wake();
                    break;
                }
            }
        }
    }
    fn finish_cancellation(&self) {
        {
            // A current poll may still own the future. Do not discard the
            // cancellation notification just because the mutex is busy.
            let mut fut = self.fut.lock();
            if matches!(*fut, JoinInner::Future { .. }) {
                *fut = JoinInner::Return {
                    val: Err(JoinError::Aborted),
                };
            }
            self.state.store(COMPLETED, Ordering::Release);
        }
        self.waker.wake();
    }
}

// SAFETY: allocate binds the callback to Self and erases the owning Arc before
// it escapes. Private fields prevent future extraction; the only replacements
// of a polled future drop it in place under its mutex.
unsafe impl<F: Future + Send + 'static> DynTask for Task<F>
where
    F::Output: Send + 'static,
{
    fn schedule_global(self: Arc<Self>) {
        Self::schedule_global(&self);
    }

    fn take_output(&self, output: &mut dyn std::any::Any) {
        if let JoinInner::Return { val } = &mut *self.fut.lock() {
            let val = std::mem::replace(val, Err(JoinError::Aborted));
            *output.downcast_mut().unwrap() = std::task::Poll::Ready(val);
        }
    }

    fn register(&self, waker: &Waker) {
        self.waker.register(waker)
    }

    fn cancel(self: Arc<Self>) {
        if self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                COMPLETED | CANCELLED => None,
                _ => Some(CANCELLED),
            })
            .is_ok()
        {
            // Idle tasks also need a worker to publish Aborted and wake the joiner.
            Self::schedule_global(&self);
        }
    }

    fn run(self: Arc<Self>) {
        let waker = futures_util::task::waker_ref(&self);
        let mut cx = Context::from_waker(&waker);

        match self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                IDLE => Some(RUNNING),
                RUNNING => Some(NOTIFIED),
                _ => None,
            }) {
            Ok(IDLE) => {
                // Winning IDLE -> RUNNING excludes another active poll, but a
                // joiner may briefly hold the future mutex in take_output.
                // Wait for it rather than discarding work and leaving RUNNING
                // set without a poll owner to complete or requeue the task.
                {
                    let mut fut = self.fut.lock();
                    // SAFETY: allocate exposes only an erased Arc, so callers
                    // cannot unwrap/move Task<F>. Private future access and
                    // in-place replacement keep F pinned until it is dropped.
                    let ready = unsafe { Pin::new_unchecked(&mut *fut) }
                        .poll(&mut cx)
                        .is_ready();
                    if ready {
                        self.state.store(COMPLETED, Ordering::Release);
                        self.waker.wake()
                    } else {
                        // Unlock before publishing IDLE: the next worker must
                        // be able to acquire the future when it takes the task.
                        drop(fut);
                        match self
                            .state
                            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                                matches!(state, RUNNING | NOTIFIED).then_some(IDLE)
                            }) {
                            Ok(NOTIFIED) => Self::schedule_global(&self),
                            Err(CANCELLED) => {
                                self.finish_cancellation();
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(CANCELLED) => {
                self.finish_cancellation();
            }
            // Ok(RUNNING) recorded the notification; leave the current poll
            // owner responsible for requeueing it if the future stays pending.
            _ => {}
        }
    }

    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<dyn DynTask>> {
        <Self as FatLinked<dyn DynTask>>::get_link(self)
    }
}

#[cfg(test)]
mod tests;

impl<F: Future + Send + 'static> Task<F>
where
    F::Output: Send + 'static,
{
    /// Allocate and erase before exposing the task: a typed Arc could otherwise
    /// be unwrapped and moved after polling a !Unpin future.
    pub(crate) fn allocate(handle: Arc<MTRuntime>, fut: F) -> Arc<dyn DynTask> {
        Arc::new(Self::new(handle, fut))
    }

    fn new(handle: Arc<MTRuntime>, fut: F) -> Self {
        Self {
            // SAFETY: this is Self's link, projected by the impl below. No
            // caller can substitute another concrete type's inverse callback.
            link: unsafe { FatLink::new::<Self>() },
            fut: Mutex::new(JoinInner::new(fut)),
            handle,
            state: AtomicU8::new(0),
            waker: AtomicWaker::new(),
        }
    }
}

// SAFETY: link is a direct private field initialized only with Self's callback.
// Subtracting its offset recovers the original Task<F> allocation and coercing
// that pointer supplies the matching Task<F> vtable, including size/drop glue.
unsafe impl<F: Future + Send + 'static> FatLinked<dyn DynTask> for Task<F>
where
    F::Output: Send + 'static,
{
    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<dyn DynTask>> {
        NonNull::new_unchecked(addr_of!((*self).link).cast_mut())
    }

    unsafe fn get_value(ptr: NonNull<FatLink<dyn DynTask>>) -> *const dyn DynTask {
        let offset = offset_of!(Self, link);
        ptr.as_ptr().byte_sub(offset).cast::<Self>().cast_const() as _
    }
}

// impl Wake so our task can be used as a waker
impl<F: Future + Send + 'static> futures_util::task::ArcWake for Task<F>
where
    F::Output: Send + 'static,
{
    fn wake_by_ref(arc_self: &Arc<Self>) {
        HANDLE.with(|x| match &*x.borrow() {
            // fast path, the current executor is part of the task's runtime
            Some(exe) if Arc::ptr_eq(&exe.shared, &arc_self.handle) => {
                exe.schedule_local(arc_self.clone() as Arc<dyn DynTask>)
            }
            // slow path, global schedule
            _ => Self::schedule_global(arc_self),
        });
    }
}
