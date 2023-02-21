use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Waker},
};

use arc_dyn::ThinArc;
use futures_util::{task::AtomicWaker, Future};
use pin_lock::PinLockGuard;
use pin_queue::AlreadyInsertedError;

use crate::{
    join_handle::{FutureWithOutput, JoinInner},
    sync_slot_map::ReadySlot,
    SharedHandle, HANDLE,
};

// our Task type that stores the intrusive pointers and our futures
pin_project_lite::pin_project!(
    pub(crate) struct Task<F: ?Sized> {
        // intrusive pointers
        #[pin]
        pub(crate) intrusive: pin_queue::Intrusive<QueueTypes>,

        // pointer to the runtime handle
        pub(crate) handle: Arc<SharedHandle>,

        // who should be notified of this task completing
        pub(crate) waker: AtomicWaker,

        // the future for this task
        #[pin]
        pub(crate) fut: pin_lock::PinLock<F>,
    }
);

impl Task<dyn FutureWithOutput> {
    pub(crate) fn schedule_global(
        task: Pin<ThinArc<Self>>,
    ) -> Result<(), AlreadyInsertedError<ThinArc<Self>>> {
        task.handle
            .global_queue
            .lock()
            .unwrap()
            .push_back(task.clone())?;

        // if we push to the global queue, we should try wake up a worker to process it
        if let ReadySlot::Ready(index) = task.handle.parked_workers.lock().pop() {
            task.handle
                .workers
                .get()
                .expect("runtime not initialised properly")[index]
                .unpark();
        }

        Ok(())
    }
}

impl<F: Future> Task<JoinInner<F>> {
    pub(crate) fn new(handle: Arc<SharedHandle>, fut: F) -> Self {
        Self {
            intrusive: pin_queue::Intrusive::new(),
            fut: pin_lock::PinLock::new(JoinInner::new(fut)),
            handle,
            waker: AtomicWaker::new(),
        }
    }
}

impl Task<dyn FutureWithOutput> {
    pub(crate) fn lock_fut(
        task: &PinTask<dyn FutureWithOutput>,
    ) -> PinLockGuard<'_, dyn FutureWithOutput> {
        task.as_ref().project_ref().fut.lock()
    }
    pub(crate) fn register(task: &PinTask<dyn FutureWithOutput>, waker: &Waker) {
        task.waker.register(waker)
    }

    pub(crate) fn run(task: PinTask<dyn FutureWithOutput>) {
        // impl ThinWake so our task can be used as a waker
        impl arc_dyn::pin_queue::ThinWake for Task<dyn FutureWithOutput> {
            fn wake(task: PinTask<dyn FutureWithOutput>) {
                let _ = HANDLE.with(|x| match &*x.borrow() {
                    // fast path, the current executor is part of the task's runtime
                    Some(exe) if Arc::ptr_eq(&exe.shared, &task.handle) => exe.schedule_local(task),
                    // slow path, global schedule
                    _ => Self::schedule_global(task),
                });
            }
        }

        let waker = Waker::from(task.clone());
        let mut cx = Context::from_waker(&waker);
        let mut fut = task.as_ref().project_ref().fut.lock();
        if fut.as_mut().poll(&mut cx).is_ready() {
            task.waker.wake()
        }
    }
}

// pin-queue stuff
pub(crate) type PinTask<F> = Pin<ThinArc<Task<F>>>;
pub(crate) type QueueTypes = dyn pin_queue::Types<
    Id = pin_queue::id::Checked,
    Key = Key,
    Pointer = ThinArc<Task<dyn FutureWithOutput>>,
>;
pub(crate) type PinQueue = pin_queue::PinQueue<QueueTypes>;

pub(crate) struct Key;
impl pin_queue::GetIntrusive<QueueTypes> for Key {
    fn get_intrusive(
        p: Pin<&Task<dyn FutureWithOutput>>,
    ) -> Pin<&pin_queue::Intrusive<QueueTypes>> {
        p.project_ref().intrusive
    }
}
