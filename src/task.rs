use std::{
    pin::Pin,
    ptr::{addr_of, NonNull},
    sync::{atomic::AtomicBool, Arc},
    task::{Context, Poll, Waker},
};

use futures_util::{task::AtomicWaker, Future};

use crate::{
    join_handle::{JoinInner, JoinInnerProj},
    linked_list::FatLink,
    sync_slot_map::ReadySlot,
    MTRuntime, WorkerThreadWaker, HANDLE,
};

// our Task type that stores the intrusive pointers and our futures
pin_project_lite::pin_project!(
    pub(crate) struct Task<F: Future> {
        // intrusive pointers
        pub(crate) link: FatLink,

        // pointer to the runtime handle
        pub(crate) handle: Arc<MTRuntime>,

        // who should be notified of this task completing
        pub(crate) complete: AtomicBool,
        pub(crate) waker: AtomicWaker,
        // the future for this task
        #[pin]
        pub(crate) fut: pin_lock::PinLock<JoinInner<F>>,
    }
);

pub(crate) trait DynTask: Send + Sync + 'static {
    fn schedule_global(self: Arc<Self>);
    fn register(&self, waker: &Waker);
    fn run(self: Arc<Self>);

    /// # Safety
    /// the output pointer must point to a valid `Poll<Output>` that is writeable and currently set to `Pending`
    unsafe fn take_output(&self, output: *mut ());
    #[cfg(debug_assertions)]
    fn output_type_id(&self) -> std::any::TypeId;

    unsafe fn get_link(self: *const Self) -> NonNull<FatLink>;
}

impl<F: Future + Send + 'static> DynTask for Task<F>
where
    F::Output: Send + 'static,
{
    fn schedule_global(self: Arc<Self>) {
        self.handle
            .global_queue
            .lock()
            .unwrap()
            .push_back(self.clone());

        // if we push to the global queue, we should try wake up a worker to process it
        // if it's already locked, then we know another worker is already being woken up.
        if let Some(mut parked) = self.handle.parked_workers.try_lock() {
            loop {
                if let ReadySlot::Ready(index) = parked.pop() {
                    let worker = &self
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
                    self.handle.io_handle.wake();
                    break;
                }
            }
        }
    }

    unsafe fn take_output(&self, output: *mut ()) {
        let this = Pin::new_unchecked(self);
        let mut fut = this.as_ref().project_ref().fut.lock();
        let output = output as *mut Poll<F::Output>;
        if let JoinInnerProj::Return { val } = fut.as_mut().project() {
            if let Some(val) = val.take() {
                // SAFETY: enforced by the caller of this unsafe fn that `output` is `Poll<Output>` and is writeable
                unsafe { output.write(Poll::Ready(val)) }
            }
        }
    }

    fn register(&self, waker: &Waker) {
        self.waker.register(waker)
    }

    fn run(self: Arc<Self>) {
        let waker = self.clone().into();
        let mut cx = Context::from_waker(&waker);
        // if it's locked, then it's already being polled or complete. we don't care
        // SAFETY: We never call `Arc::into_inner`/`Arc::get_mut`. Because of this, the future
        // is always pinned and never moved until the Arc deallocates.
        if let Some(mut fut) = unsafe { Pin::new_unchecked(&self.fut) }.try_lock() {
            if fut.as_mut().poll(&mut cx).is_ready() {
                self.complete
                    .store(true, std::sync::atomic::Ordering::Release);
                self.waker.wake()
            }
        }
    }

    #[cfg(debug_assertions)]
    fn output_type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<F::Output>()
    }

    unsafe fn get_link(self: *const Self) -> NonNull<FatLink> {
        NonNull::new_unchecked(addr_of!((*self).link).cast_mut())
    }
}

impl<F: Future + Send + 'static> Task<F>
where
    F::Output: Send + 'static,
{
    pub(crate) fn new(handle: Arc<MTRuntime>, fut: F) -> Self {
        Self {
            link: FatLink::new::<F>(),
            fut: pin_lock::PinLock::new(JoinInner::new(fut)),
            handle,
            complete: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }
}

// impl ThinWake so our task can be used as a waker
impl<F: Future + Send + 'static> std::task::Wake for Task<F>
where
    F::Output: Send + 'static,
{
    fn wake(self: Arc<Self>) {
        HANDLE.with(|x| match &*x.borrow() {
            // fast path, the current executor is part of the task's runtime
            Some(exe) if Arc::ptr_eq(&exe.shared, &self.handle) => {
                exe.schedule_local(self as Arc<dyn DynTask>)
            }
            // slow path, global schedule
            _ => self.schedule_global(),
        });
    }
}
