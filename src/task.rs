use std::{
    pin::Pin,
    ptr::NonNull,
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

/// our Task type that stores the intrusive pointers and our futures
pub(crate) struct Task<F: Future> {
    /// intrusive pointers
    pub(crate) link: FatLink<dyn DynTask>,

    /// pointer to the runtime handle
    pub(crate) handle: Arc<MTRuntime>,

    /// who should be notified of this task completing
    pub(crate) state: AtomicU8,
    pub(crate) waker: AtomicWaker,

    /// the future for this task
    pub(crate) fut: Mutex<JoinInner<F>>,
}

pub(crate) trait DynTask: Send + Sync + 'static {
    fn schedule_global(self: Arc<Self>);
    fn run(self: Arc<Self>);

    fn register(&self, waker: &Waker);
    fn cancel(self: Arc<Self>);

    fn take_output(&self, output: &mut dyn std::any::Any);

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

impl<F: Future + Send + 'static> DynTask for Task<F>
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
                    // SAFETY: We never call `Arc::into_inner`/`Arc::get_mut`. Because of this, the future
                    // is always pinned and never moved until the Arc deallocates.
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
    pub(crate) fn new(handle: Arc<MTRuntime>, fut: F) -> Self {
        Self {
            link: FatLink::new::<Self>(),
            fut: Mutex::new(JoinInner::new(fut)),
            handle,
            state: AtomicU8::new(0),
            waker: AtomicWaker::new(),
        }
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
