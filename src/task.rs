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
    join_handle::{JoinError, JoinInner, Request},
    linked_list::{FatLink, FatLinked},
    sync_slot_map::ReadySlot,
    MTRuntime, WorkerThreadWaker, HANDLE,
};

const IDLE: u8 = 0;
const RUNNING: u8 = 1;
const COMPLETED: u8 = 1;
const CANCELLED: u8 = 2;

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
    fn cancel(&self);

    fn take_output(&self, output: &mut dyn Request);

    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<dyn DynTask>>;
}

impl<F: Future + Send + 'static> Task<F>
where
    F::Output: Send + 'static,
{
    fn schedule_global(this: &Arc<Self>) {
        this.handle
            .global_queue
            .lock()
            .unwrap()
            .push_back(this.clone());

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
}

impl<F: Future + Send + 'static> DynTask for Task<F>
where
    F::Output: Send + 'static,
{
    fn schedule_global(self: Arc<Self>) {
        Self::schedule_global(&self);
    }

    fn take_output(&self, output: &mut dyn Request) {
        if let JoinInner::Return { val } = &mut *self.fut.lock() {
            output.push(&mut *val);
        }
    }

    fn register(&self, waker: &Waker) {
        self.waker.register(waker)
    }

    fn cancel(&self) {
        if let Some(mut fut) = self.fut.try_lock() {
            if let JoinInner::Future { .. } = *fut {
                *fut = JoinInner::Return {
                    val: Err(JoinError::Aborted),
                }
            }
        } else {
            match self.state.compare_exchange(
                RUNNING,
                CANCELLED,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {}
                Err(_) => todo!(),
            }
        }
    }

    fn run(self: Arc<Self>) {
        let waker = futures_util::task::waker_ref(&self);
        let mut cx = Context::from_waker(&waker);

        match self
            .state
            .compare_exchange(IDLE, RUNNING, Ordering::Acquire, Ordering::Acquire)
        {
            Ok(_) => {
                // if it's locked, then it's already being polled or complete. we don't care
                if let Some(mut fut) = self.fut.try_lock() {
                    // SAFETY: We never call `Arc::into_inner`/`Arc::get_mut`. Because of this, the future
                    // is always pinned and never moved until the Arc deallocates.
                    let mut fut = unsafe { Pin::new_unchecked(&mut *fut) };

                    if fut.as_mut().poll(&mut cx).is_ready() {
                        self.state.store(COMPLETED, Ordering::Release);
                        self.waker.wake()
                    } else if let Err(CANCELLED) = self.state.compare_exchange(
                        RUNNING,
                        IDLE,
                        Ordering::Acquire,
                        Ordering::Acquire,
                    ) {
                        fut.set(JoinInner::Return {
                            val: Err(JoinError::Aborted),
                        });
                    }
                }
            }
            Err(CANCELLED) => {
                // if it's locked, then it's already being polled or complete. we don't care
                if let Some(mut fut) = self.fut.try_lock() {
                    if matches!(*fut, JoinInner::Future { .. }) {
                        *fut = JoinInner::Return {
                            val: Err(JoinError::Aborted),
                        };
                    }
                    self.waker.wake()
                }
            }
            Err(_) => {}
        }
    }

    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<dyn DynTask>> {
        <Self as FatLinked<dyn DynTask>>::get_link(self)
    }
}

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
