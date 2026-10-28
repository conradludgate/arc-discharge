use super::TaskAdapter;
use crate::{
    task::{DynTask, Task},
    JoinError, MTRuntime,
};
use intrusive_collections::LinkedList;
use std::{
    future::Future,
    marker::PhantomPinned,
    panic::{catch_unwind, AssertUnwindSafe},
    pin::Pin,
    sync::{
        atomic::{AtomicPtr, AtomicUsize, Ordering},
        Arc, Barrier, Mutex, OnceLock,
    },
    task::{Context, Poll},
};

// Real runtime state and I/O handle, with no background workers. Each test drives
// tasks explicitly and releases its queues, so Miri can check leaks as well as
// ownership/pinning. No task, list or I/O implementation is replaced by a mock.
fn runtime() -> Arc<MTRuntime> {
    let (driver, handle) = crate::io::IODriver::new();
    Arc::new(MTRuntime {
        global_queue: Mutex::new(LinkedList::new(TaskAdapter::new())),
        workers: OnceLock::from(Vec::new().into_boxed_slice()),
        parked_workers: crate::sync_slot_map::SyncSlotMap::new(0),
        io_handle: Arc::new(handle),
        io_driver: Mutex::new(driver),
    })
}

fn pop(runtime: &MTRuntime) -> Option<Arc<dyn DynTask>> {
    runtime.global_queue.lock().unwrap().pop_front()
}

fn output<T: PartialEq + std::fmt::Debug + 'static>(task: &Arc<dyn DynTask>, expected: T) {
    let mut output = Poll::<Result<T, JoinError>>::Pending;
    task.take_output(&mut output);
    match output {
        Poll::Ready(Ok(actual)) => assert_eq!(actual, expected),
        _ => panic!("missing task output"),
    }
}

#[test]
fn heterogeneous_tasks_round_trip_and_reject_duplicate_membership() {
    let runtime = runtime();
    let number = Task::allocate(runtime.clone(), async { 1usize });
    let string = Task::allocate(runtime.clone(), async { String::from("four") });
    let mut list = LinkedList::new(TaskAdapter::new());
    list.push_back(number.clone());
    list.push_back(string.clone());
    assert_eq!(Arc::strong_count(&number), 2);
    // Upstream rejects duplicate insertion by panicking. It must release only
    // the attempted insertion's Arc and preserve the existing node and vtable.
    assert!(catch_unwind(AssertUnwindSafe(|| list.push_back(number.clone()))).is_err());
    assert_eq!(Arc::strong_count(&number), 2);
    let first = list.pop_front().unwrap();
    let second = list.pop_front().unwrap();
    // Trait-object vtable addresses need not be identical after reconstruction.
    assert!(std::ptr::addr_eq(Arc::as_ptr(&first), Arc::as_ptr(&number)));
    assert!(std::ptr::addr_eq(
        Arc::as_ptr(&second),
        Arc::as_ptr(&string)
    ));
    assert!(list.is_empty());
    first.run();
    second.run();
    output(&number, 1usize);
    output(&string, String::from("four"));
    assert_eq!(Arc::strong_count(&number), 1);
    assert_eq!(Arc::strong_count(&string), 1);
}

#[repr(align(4096))]
struct SelfReferential {
    saved: AtomicPtr<usize>,
    payload: usize,
    remaining: usize,
    drops: Arc<AtomicUsize>,
    _pin: PhantomPinned,
}

impl SelfReferential {
    fn new(drops: Arc<AtomicUsize>) -> Self {
        Self {
            saved: AtomicPtr::new(std::ptr::null_mut()),
            payload: 17,
            remaining: 2,
            drops,
            _pin: PhantomPinned,
        }
    }

    fn check(&self) {
        let saved = self.saved.load(Ordering::Relaxed);
        if !saved.is_null() {
            assert_eq!(saved.cast_const(), &self.payload as *const usize);
            // SAFETY: once polled, this !Unpin future must remain at the same
            // address until Drop. Dereference as well as comparing addresses.
            assert_eq!(unsafe { *saved }, self.payload);
        }
    }
}

impl Future for SelfReferential {
    type Output = usize;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<usize> {
        self.check();
        let saved = self.saved.load(Ordering::Relaxed);
        if saved.is_null() {
            self.saved.store(
                &self.payload as *const usize as *mut usize,
                Ordering::Relaxed,
            );
        }
        // SAFETY: only the counter changes; no pinned field is moved.
        let this = unsafe { self.get_unchecked_mut() };
        if this.remaining == 0 {
            Poll::Ready(this.payload)
        } else {
            this.remaining -= 1;
            Poll::Pending
        }
    }
}

impl Drop for SelfReferential {
    fn drop(&mut self) {
        self.check();
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn pinned_future_survives_reinsertion_and_thread_transfer() {
    let runtime = runtime();
    let drops = Arc::new(AtomicUsize::new(0));
    let task = Task::allocate(runtime, SelfReferential::new(drops.clone()));
    let mut list = LinkedList::new(TaskAdapter::new());
    list.push_back(task.clone());
    list.pop_front().unwrap().run();
    list.push_back(task.clone());
    let mut list = std::thread::spawn(move || {
        list.pop_front().unwrap().run();
        list
    })
    .join()
    .unwrap();
    list.push_back(task.clone());
    list.pop_front().unwrap().run();
    output(&task, 17usize);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn clear_and_drop_release_the_last_owner_of_a_pinned_future() {
    let runtime = runtime();
    let drops = Arc::new(AtomicUsize::new(0));
    for clear in [true, false] {
        let task = Task::allocate(runtime.clone(), SelfReferential::new(drops.clone()));
        task.clone().run();
        let weak = Arc::downgrade(&task);
        let mut list = LinkedList::new(TaskAdapter::new());
        list.push_back(task);
        if clear {
            list.clear();
        }
        drop(list);
        assert!(weak.upgrade().is_none());
    }
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn retained_alias_can_be_polled_after_clear() {
    let runtime = runtime();
    let drops = Arc::new(AtomicUsize::new(0));
    let task = Task::allocate(runtime, SelfReferential::new(drops.clone()));
    task.clone().run();
    let mut list = LinkedList::new(TaskAdapter::new());
    list.push_back(task.clone());
    list.clear();
    task.clone().run();
    task.clone().run();
    output(&task, 17usize);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn cancellation_drops_a_pinned_future_in_place() {
    let runtime = runtime();
    let drops = Arc::new(AtomicUsize::new(0));
    let task = Task::allocate(runtime.clone(), SelfReferential::new(drops.clone()));
    task.clone().run();
    task.clone().cancel();
    pop(&runtime).unwrap().run();
    let mut result = Poll::<Result<usize, JoinError>>::Pending;
    task.take_output(&mut result);
    assert!(matches!(result, Poll::Ready(Err(JoinError::Aborted))));
    assert!(pop(&runtime).is_none());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn concurrent_wakes_and_dequeues_preserve_single_membership() {
    let runtime = runtime();
    let saved = Arc::new(Mutex::new(None));
    let saved_waker = saved.clone();
    let polls = Arc::new(AtomicUsize::new(0));
    let polled = polls.clone();
    let task = Task::allocate(
        runtime.clone(),
        std::future::poll_fn(move |cx| {
            if polled.fetch_add(1, Ordering::Relaxed) == 0 {
                *saved_waker.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            } else {
                Poll::Ready(5usize)
            }
        }),
    );
    task.clone().run();
    let start = Arc::new(Barrier::new(3));
    let done = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let start = start.clone();
        let done = done.clone();
        let waker = saved.lock().unwrap().as_ref().unwrap().clone();
        threads.push(std::thread::spawn(move || {
            start.wait();
            for _ in 0..16 {
                waker.wake_by_ref();
                std::thread::yield_now();
            }
            done.fetch_add(1, Ordering::Release);
        }));
    }
    start.wait();
    while done.load(Ordering::Acquire) < 2 {
        while let Some(task) = pop(&runtime) {
            task.run();
        }
        std::thread::yield_now();
    }
    for thread in threads {
        thread.join().unwrap();
    }
    while let Some(task) = pop(&runtime) {
        task.run();
    }
    output(&task, 5usize);
    assert_eq!(polls.load(Ordering::Relaxed), 2);
    saved.lock().unwrap().take();
    assert!(pop(&runtime).is_none());
}
