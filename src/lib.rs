#![feature(arbitrary_self_types)]
#![feature(exclusive_wrapper)]

use intrusive_collections::XorLinkedList;
use linked_list::TaskAdapter;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::task::{Context, Wake, Waker};
use std::thread::{self, available_parallelism, Thread};
use sync_slot_map::SyncSlotMap;
use task::{DynTask, Task};

pub use join_handle::{JoinError, JoinHandle};

mod bits;
mod io;
mod join_handle;
mod linked_list;
pub mod net;
mod sync_slot_map;
mod task;

/// Multithreaded runtime
pub struct MTRuntime {
    global_queue: Mutex<XorLinkedList<TaskAdapter>>,
    workers: OnceLock<Box<[Worker]>>,
    parked_workers: SyncSlotMap,
    io_handle: Arc<io::Handle>,
    io_driver: Mutex<io::IODriver>,
}

#[derive(Debug)]
pub struct Worker {
    #[allow(dead_code)]
    thread: Thread,
    parker: Mutex<Option<WorkerThreadWaker>>,
    unparker: Condvar,
}

impl MTRuntime {
    /// Make a new multi-threaded runtime with `n` background threads
    pub fn new(n: NonZeroUsize) -> Arc<Self> {
        let (driver, handle) = io::IODriver::new();
        let shared = Arc::new(MTRuntime {
            global_queue: Mutex::new(XorLinkedList::new(TaskAdapter::new())),
            workers: OnceLock::new(),
            parked_workers: SyncSlotMap::new(n.get()),
            io_handle: Arc::new(handle),
            io_driver: Mutex::new(driver),
        });

        // no init step
        shared.init_workers(n);

        shared
    }

    /// Make a new multi-threaded runtime with a thread per logical CPU
    pub fn thread_per_core() -> Arc<Self> {
        let cores = available_parallelism().unwrap();
        Self::new(cores)
    }

    /// Make a new multi-threaded runtime with `n` background threads
    fn init_workers(self: &Arc<Self>, worker_init: NonZeroUsize) {
        let mut workers = Vec::with_capacity(worker_init.get());
        for i in 0..worker_init.get() {
            let exec = ExecutorHandle {
                worker_index: i,
                shared: self.clone(),
                local_queue: RefCell::new(VecDeque::with_capacity(32)),
            };
            let thread = thread::Builder::new()
                .name(format!("arc-dispatch-worker-{i}"))
                .spawn(|| {
                    // park until we can start
                    while exec.shared.workers.get().is_none() {
                        thread::park();
                    }

                    let exec = Rc::new(exec);
                    exec.run_forever();
                })
                .unwrap();
            workers.push({
                Worker {
                    thread: thread.thread().clone(),
                    parker: Mutex::new(None),
                    unparker: Condvar::new(),
                }
            });
        }

        self.workers.set(workers.into_boxed_slice()).unwrap();

        for i in &**self.workers.get().unwrap() {
            i.thread.unpark();
        }
    }

    /// Block the current thread and wait for the future to complete.
    pub fn block_on<F: Future>(self: &Arc<Self>, f: F) -> F::Output {
        let fake_exec = Rc::new(ExecutorHandle {
            worker_index: usize::MAX,
            shared: self.clone(),
            local_queue: RefCell::new(VecDeque::with_capacity(0)),
        });
        if HANDLE.with(|x| x.borrow_mut().replace(fake_exec)).is_some() {
            panic!(
                "block on called within the context of another runtime. this is most likely a bug"
            );
        }

        struct ThreadWaker {
            thread: Thread,
        }

        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.wake_by_ref()
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.thread.unpark()
            }
        }

        let waker = Waker::from(Arc::new(ThreadWaker {
            thread: std::thread::current(),
        }));
        let mut cx = Context::from_waker(&waker);
        futures_util::pin_mut!(f);

        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                break v;
            }
            thread::park();
        }
    }

    /// spawn a task onto this runtime
    pub fn spawn<F: Future + Send + 'static>(self: &Arc<Self>, f: F) -> JoinHandle<F::Output>
    where
        F::Output: Send + 'static,
    {
        let task = Task::new(self.clone(), f);
        let (task, join) = JoinHandle::new(task);

        HANDLE.with(|x| {
            if let Some(local) = x
                .borrow()
                .as_ref()
                .filter(|exec| Arc::ptr_eq(&exec.shared, self))
            {
                local.schedule_local(task)
            } else {
                task.schedule_global()
            }
        });

        join
    }
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<ExecutorHandle>>> = const { RefCell::new(None) };
}

struct ExecutorHandle {
    worker_index: usize,
    shared: Arc<MTRuntime>,
    local_queue: RefCell<VecDeque<Arc<dyn DynTask>>>,
}

impl ExecutorHandle {
    fn spawn<F: Future + Send + 'static>(&self, f: F) -> JoinHandle<F::Output>
    where
        F::Output: Send + 'static,
    {
        let task = Task::new(self.shared.clone(), f);
        let (task, join) = JoinHandle::new(task);

        self.schedule_local(task);

        join
    }

    fn schedule_local(&self, task: Arc<dyn DynTask>) {
        let mut queue = self.local_queue.borrow_mut();
        if queue.len() < queue.capacity() {
            queue.push_back(task);
        } else {
            task.schedule_global();
        }
    }

    fn run_forever(self: Rc<ExecutorHandle>) {
        let workers = self.shared.workers.get().unwrap();
        let worker = &workers[self.worker_index];
        HANDLE.with(|exec| *exec.borrow_mut() = Some(self.clone()));
        let mut counter = 0_usize;
        loop {
            counter = counter.wrapping_add(1);
            let task = if counter % 31 != 0 || self.local_queue.borrow().len() > 16 {
                self.get_local_task()
            } else {
                None
            };
            let task = task.or_else(|| self.get_many_global_tasks());
            if let Some(task) = task {
                task.run();
            } else if let Ok(mut io) = self.shared.io_driver.try_lock() {
                // *parking = Some(WorkerThreadWaker::IO);
                // drop(parking);
                io.poll_timeout(&self.shared.io_handle, None);
            } else {
                let mut parking = worker.parker.lock().unwrap();
                if parking.is_none() {
                    self.shared.parked_workers.push(self.worker_index);
                }
                *parking = Some(WorkerThreadWaker::Parked);
                *worker.unparker.wait(parking).unwrap() = None;
            }
        }
    }

    fn get_local_task(&self) -> Option<Arc<dyn DynTask>> {
        let mut queue = self.local_queue.borrow_mut();
        queue.pop_front()
    }

    fn get_many_global_tasks(&self) -> Option<Arc<dyn DynTask>> {
        let mut global = self.shared.global_queue.lock().unwrap();
        let mut queue = self.local_queue.borrow_mut();
        for _ in 0..4 {
            if let Some(next) = global.pop_front() {
                // CURRENT_GLOBAL.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                queue.push_back(next);
            }
        }
        queue.pop_front()
    }
}

#[derive(Debug)]
enum WorkerThreadWaker {
    Parked,
}

pub fn spawn<F: Future + Send + 'static>(f: F) -> JoinHandle<F::Output>
where
    F::Output: Send + 'static,
{
    HANDLE.with(|x| {
        x.borrow()
            .as_ref()
            .expect("spawn called outside of the context of a runtime")
            .spawn(f)
    })
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{atomic::AtomicUsize, Arc},
        task::Poll,
        thread,
        time::{Duration, Instant},
    };

    use futures_util::Future;

    use crate::{spawn, MTRuntime};

    #[test]
    fn timers() {
        let rt = MTRuntime::thread_per_core();

        let counter = Arc::new(AtomicUsize::new(0));
        let start = Instant::now();

        let output = rt.block_on(async {
            let counter1 = counter.clone();
            let a = spawn(async move {
                for _ in 0..10 {
                    sleep(Duration::from_secs(1)).await;
                    counter1.fetch_add(10, std::sync::atomic::Ordering::Relaxed);
                }
                42
            });

            let counter2 = counter.clone();
            let b = spawn(async move {
                for _ in 0..10 {
                    sleep(Duration::from_secs(1)).await;
                    counter2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                27
            });

            a.await.unwrap() + b.await.unwrap()
        });

        assert_eq!(output, 69);
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 110);
        assert!(
            (start.elapsed().as_secs_f64() - 10.0).abs() < 0.1,
            "time taken to complete is within 0.1 seconds"
        );
    }

    async fn sleep(duration: Duration) {
        ThreadSleeper {
            instant: Instant::now() + duration,
            spawned: false,
        }
        .await
    }

    struct ThreadSleeper {
        instant: Instant,
        spawned: bool,
    }

    impl Future for ThreadSleeper {
        type Output = ();

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<Self::Output> {
            if self.spawned || Instant::now() > self.instant {
                Poll::Ready(())
            } else {
                self.spawned = true;
                let instant = self.instant;
                let waker = cx.waker().clone();
                thread::spawn(move || {
                    thread::sleep(instant - Instant::now());
                    waker.wake()
                });
                Poll::Pending
            }
        }
    }
}
