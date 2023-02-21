#![feature(once_cell)]
use arc_dyn::ThinArc;
use join_handle::{FutureWithOutput, JoinHandle};
use pin_queue::AlreadyInsertedError;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Wake, Waker};
use std::thread::{self, Thread};
use sync_slot_map::SyncSlotMap;
use task::{PinQueue, PinTask, Task};

mod join_handle;
mod sync_slot_map;
mod task;

pub struct SharedHandle {
    global_queue: Mutex<PinQueue>,
    workers: OnceLock<Box<[Thread]>>,
    parked_workers: SyncSlotMap,
}

impl SharedHandle {
    pub fn new(workers: usize) -> Arc<Self> {
        let shared = Arc::new(SharedHandle {
            global_queue: Mutex::new(PinQueue::new(pin_queue::id::Checked::new())),
            workers: OnceLock::new(),
            parked_workers: SyncSlotMap::new(workers),
        });

        let mut w = Vec::with_capacity(workers);
        for i in 0..workers {
            let exec = ExecutorHandle {
                worker_index: i,
                shared: shared.clone(),
                local_queue: RefCell::new(VecDeque::with_capacity(32)),
            };
            let thread = thread::Builder::new()
                .name(format!("arc-dispatch-{i}"))
                .spawn(|| {
                    // park until we can start
                    while exec.shared.workers.get().is_none() {
                        thread::park();
                    }

                    let exec = Rc::new(exec);
                    exec.run_forever();
                })
                .unwrap();
            w.push(thread.thread().clone());
        }

        shared.workers.set(w.into_boxed_slice()).unwrap();
        shared
    }

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
            std::thread::park();
        }
    }
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<ExecutorHandle>>> = RefCell::new(None);
}

struct ExecutorHandle {
    worker_index: usize,
    shared: Arc<SharedHandle>,
    local_queue: RefCell<VecDeque<PinTask<dyn FutureWithOutput>>>,
}

impl ExecutorHandle {
    fn spawn<F: Future + Send + Sync + 'static>(&self, f: F) -> JoinHandle<F::Output>
    where
        F::Output: Send + Sync + 'static,
    {
        let task = Task::new(self.shared.clone(), f);
        let (task, join) = JoinHandle::new(task);

        self.schedule_local(task.clone())
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
            let mut queue = self.shared.global_queue.lock().unwrap();
            queue.push_back(task)
        }
    }

    fn run_forever(self: Rc<ExecutorHandle>) {
        HANDLE.with(|exec| *exec.borrow_mut() = Some(self.clone()));
        loop {
            // if no local tasks, get from the global queue
            let task = self.get_local_task().or_else(|| self.get_global_task());
            if let Some(task) = task {
                Task::run(task);
            } else {
                self.shared.parked_workers.push(self.worker_index);
                thread::park();
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
