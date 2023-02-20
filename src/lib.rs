use arc_dyn::ThinArc;
use futures_util::task::AtomicWaker;
use pin_queue::AlreadyInsertedError;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;

// aliases
type DynTask = Task<dyn Future<Output = ()> + Send + Sync + 'static>;
type QueueTypes =
    dyn pin_queue::Types<Id = pin_queue::id::Checked, Key = Key, Pointer = ThinArc<DynTask>>;
type PinQueue = pin_queue::PinQueue<QueueTypes>;

// our Task type that stores the intrusive pointers and our futures
pin_project_lite::pin_project!(
    struct Task<F: ?Sized> {
        // intrusive pointers
        #[pin]
        intrusive: pin_queue::Intrusive<QueueTypes>,

        // pointer to the runtime handle
        handle: Arc<SharedHandle>,

        // who should be notified of this task completing
        waker: AtomicWaker,

        // the future for this task
        #[pin]
        fut: pin_lock::PinLock<F>,
    }
);

impl DynTask {
    fn schedule_global(
        task: Pin<ThinArc<Self>>,
    ) -> Result<(), AlreadyInsertedError<ThinArc<Self>>> {
        task.handle
            .global_queue
            .lock()
            .unwrap()
            .push_back(task.clone())
    }
}

struct JoinHandle<O>(Pin<ThinArc<Task<FutureWrapper<dyn Future<Output = O>>>>>);

impl<O> Future for JoinHandle<O> {
    type Output = O;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let task = self.0.as_ref().project_ref();
        let mut fut = task.fut.lock();
        if let Some(out) = fut.as_mut().project().out.take() {
            Poll::Ready(out)
        } else {
            task.waker.register(cx.waker());
            Poll::Pending
        }
    }
}

pin_project_lite::pin_project!(
    struct FutureWrapper<F: ?Sized>
    where
        F: Future,
    {
        out: Option<F::Output>,
        #[pin]
        fut: F,
    }
);

impl<F: Future + ?Sized> Future for FutureWrapper<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.fut.poll(cx) {
            Poll::Ready(x) => {
                *this.out = Some(x);
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<F: Future> Task<FutureWrapper<F>> {
    fn new(handle: Arc<SharedHandle>, fut: F) -> Self {
        Self {
            intrusive: pin_queue::Intrusive::new(),
            fut: pin_lock::PinLock::new(FutureWrapper { out: None, fut }),
            handle,
            waker: AtomicWaker::new(),
        }
    }
}

struct Key;
impl pin_queue::GetIntrusive<QueueTypes> for Key {
    fn get_intrusive(p: Pin<&DynTask>) -> Pin<&pin_queue::Intrusive<QueueTypes>> {
        p.project_ref().intrusive
    }
}

struct SharedHandle {
    global_queue: Mutex<PinQueue>,
}

struct ExecutorHandle {
    shared: Arc<SharedHandle>,
    local_queue: RefCell<VecDeque<Pin<ThinArc<DynTask>>>>,
}

impl ExecutorHandle {
    fn spawn<F: Future + Send + Sync + 'static>(&self, f: F)
    where
        F::Output: Send + Sync + 'static,
    {
        let task = ThinArc::pin(Task::new(self.shared.clone(), f));
        self.schedule_local(task)
            .expect("new task cannot exist in another queue");
    }

    fn schedule_local(
        &self,
        task: Pin<ThinArc<DynTask>>,
    ) -> Result<(), AlreadyInsertedError<ThinArc<DynTask>>> {
        let mut queue = self.local_queue.borrow_mut();
        if queue.len() < queue.capacity() {
            queue.push_back(task);
            Ok(())
        } else {
            let mut queue = self.shared.global_queue.lock().unwrap();
            queue.push_back(task)
        }
    }

    fn run(&self) {
        loop {
            let task = {
                let mut queue = self.local_queue.borrow_mut();
                queue.pop_front()
            };
            // if no local tasks, get from the global queue
            let task = task.or_else(|| self.shared.global_queue.lock().unwrap().pop_front());
            if let Some(task) = task {
                let waker = Waker::from(task.clone());
                let mut cx = Context::from_waker(&waker);
                let mut fut = task.as_ref().project_ref().fut.lock();
                let _ = fut.as_mut().poll(&mut cx);
            } else {
                thread::park();
            }
        }
    }
}

thread_local! {
    static HANDLE: RefCell<Option<Rc<ExecutorHandle>>> = RefCell::new(None);
}

impl arc_dyn::pin_queue::ThinWake for DynTask {
    fn wake(task: Pin<ThinArc<DynTask>>) {
        let _ = HANDLE.with(|x| match &*x.borrow() {
            // fast path, the current executor is part of the task's runtime
            Some(exe) if Arc::ptr_eq(&exe.shared, &task.handle) => exe.schedule_local(task),
            // slow path, global schedule
            _ => DynTask::schedule_global(task),
        });
    }
}
