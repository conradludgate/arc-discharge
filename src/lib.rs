use arc_dyn::ThinArc;
use join_handle::{FutureWithOutput, JoinHandle};
use pin_queue::AlreadyInsertedError;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::task::{Context, Wake, Waker};
use std::thread::{self, Thread};
use sync_slot_map::SyncSlotMap;
use task::{PinQueue, PinTask, Task};

mod bits;
mod io;
mod join_handle;
pub mod net;
mod sync_slot_map;
mod task;

/// Multithreaded runtime
pub struct MTRuntime {
    global_queue: Mutex<PinQueue>,
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
            global_queue: Mutex::new(PinQueue::new(pin_queue::id::Checked::new())),
            workers: OnceLock::new(),
            parked_workers: SyncSlotMap::new(n.get()),
            io_handle: Arc::new(handle),
            io_driver: Mutex::new(driver),
        });

        // no init step
        let init = (0..n.get()).map(|i| (i, || {})).collect();
        shared.init_workers(init);

        shared
    }

    /// Make a new multi-threaded runtime with a thread per logical CPU, with the appropriate
    /// CPU afinity set.
    pub fn thread_per_core() -> Arc<Self> {
        let cores = core_affinity::get_core_ids().unwrap();

        let (driver, handle) = io::IODriver::new();
        let shared = Arc::new(MTRuntime {
            global_queue: Mutex::new(PinQueue::new(pin_queue::id::Checked::new())),
            workers: OnceLock::new(),
            parked_workers: SyncSlotMap::new(cores.len()),
            io_handle: Arc::new(handle),
            io_driver: Mutex::new(driver),
        });

        let init = cores
            .into_iter()
            .map(|c| {
                move || {
                    core_affinity::set_for_current(c);
                }
            })
            .enumerate()
            .collect();
        shared.init_workers(init);

        shared
    }

    /// Make a new multi-threaded runtime with `n` background threads
    fn init_workers(self: &Arc<Self>, worker_init: Vec<(usize, impl FnOnce() + Send + 'static)>) {
        let mut workers = Vec::with_capacity(worker_init.len());
        for (i, init) in worker_init {
            let exec = ExecutorHandle {
                worker_index: i,
                shared: self.clone(),
                local_queue: RefCell::new(VecDeque::with_capacity(32)),
            };
            let thread = thread::Builder::new()
                .name(format!("arc-dispatch-worker-{i}"))
                .spawn(|| {
                    init();

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
    pub fn spawn<F: Future + Send + Sync + 'static>(self: &Arc<Self>, f: F) -> JoinHandle<F::Output>
    where
        F::Output: Send + Sync + 'static,
    {
        let task = Task::new(self.clone(), f);
        let (task, join) = JoinHandle::new(task);

        HANDLE
            .with(|x| {
                if let Some(local) = x
                    .borrow()
                    .as_ref()
                    .filter(|exec| Arc::ptr_eq(&exec.shared, self))
                {
                    local.schedule_local(task)
                } else {
                    let mut queue = self.global_queue.lock().unwrap();
                    queue.push_back(task)
                }
            })
            .expect("new task cannot exist in another queue");

        join
    }
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<ExecutorHandle>>> = RefCell::new(None);
}

struct ExecutorHandle {
    worker_index: usize,
    shared: Arc<MTRuntime>,
    local_queue: RefCell<VecDeque<PinTask<dyn FutureWithOutput>>>,
}

impl ExecutorHandle {
    fn spawn<F: Future + Send + Sync + 'static>(&self, f: F) -> JoinHandle<F::Output>
    where
        F::Output: Send + Sync + 'static,
    {
        let task = Task::new(self.shared.clone(), f);
        let (task, join) = JoinHandle::new(task);

        self.schedule_local(task)
            .expect("new task cannot exist in another queue");

        join
    }

    fn schedule_local(
        &self,
        task: PinTask<dyn FutureWithOutput>,
    ) -> Result<(), AlreadyInsertedError<ThinArc<Task<dyn FutureWithOutput>>>> {
        let mut queue = self.local_queue.borrow_mut();
        if queue.len() < queue.capacity() {
            queue.push_back(task);
            Ok(())
        } else {
            Task::schedule_global(task)
        }
    }

    fn run_forever(self: Rc<ExecutorHandle>) {
        let workers = self.shared.workers.get().unwrap();
        let worker = &workers[self.worker_index];
        HANDLE.with(|exec| *exec.borrow_mut() = Some(self.clone()));
        loop {
            // if no local tasks, get from the global queue
            let task = self.get_local_task().or_else(|| self.get_global_task());
            if let Some(task) = task {
                Task::run(task);
            } else {
                let mut parking = worker.parker.lock().unwrap();
                self.shared.parked_workers.push(self.worker_index);
                if let Ok(mut io) = self.shared.io_driver.try_lock() {
                    *parking = Some(WorkerThreadWaker::IO);
                    drop(parking);
                    io.poll_timeout(&self.shared.io_handle, None);
                } else {
                    *parking = Some(WorkerThreadWaker::Parked);
                    drop(worker.unparker.wait(parking).unwrap());
                }
            }
        }
    }

    fn get_local_task(&self) -> Option<PinTask<dyn FutureWithOutput>> {
        let mut queue = self.local_queue.borrow_mut();
        queue.pop_front()
    }

    fn get_global_task(&self) -> Option<PinTask<dyn FutureWithOutput>> {
        self.shared.global_queue.lock().unwrap().pop_front()
    }
}

#[derive(Debug)]
enum WorkerThreadWaker {
    Parked,
    IO,
}

pub fn spawn<F: Future + Send + Sync + 'static>(f: F) -> JoinHandle<F::Output>
where
    F::Output: Send + Sync + 'static,
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

            a.await + b.await
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
