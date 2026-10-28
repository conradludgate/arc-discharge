use super::*;
use crate::{linked_list::FatLink, MTRuntime};
use std::{
    num::NonZeroUsize,
    ptr::NonNull,
    sync::{mpsc, Mutex, OnceLock},
    task::{Wake, Waker},
};

// This waker synchronously waits for a real JoinHandle poll. Run that poll on a
// helper thread so the pre-fix deadlock becomes a bounded failure, not a hung
// test process. The helper is joined after Task::run has unwound/unlocked.
struct PollDuringWake {
    join: Mutex<JoinHandle<usize>>,
    helper: Mutex<Option<std::thread::JoinHandle<()>>>,
    result: Mutex<Option<Result<usize, JoinError>>>,
}

impl Wake for PollDuringWake {
    fn wake(self: Arc<Self>) {
        let (tx, rx) = mpsc::channel();
        let poller = self.clone();
        let helper = std::thread::spawn(move || {
            let mut join = poller.join.lock().unwrap();
            let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
            let result = Pin::new(&mut *join).poll(&mut cx);
            let _ = tx.send(result);
        });
        *self.helper.lock().unwrap() = Some(helper);
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("join polling blocked inside the completion wake callback");
        match result {
            Poll::Ready(result) => *self.result.lock().unwrap() = Some(result),
            Poll::Pending => panic!("completion wake did not expose the join result"),
        }
    }
}

fn poll_during_completion_wake(
    future: impl Future<Output = usize> + Send + 'static,
    abort: bool,
) -> Result<usize, JoinError> {
    // Real runtime/I/O state, manually driven: no background worker can complete
    // the task before this test has registered its waker.
    let (driver, handle) = crate::io::IODriver::new();
    let runtime = Arc::new(MTRuntime {
        global_queue: Mutex::new(intrusive_collections::LinkedList::new(
            crate::linked_list::TaskAdapter::new(),
        )),
        workers: OnceLock::from(Vec::new().into_boxed_slice()),
        parked_workers: crate::sync_slot_map::SyncSlotMap::new(0),
        io_handle: Arc::new(handle),
        io_driver: Mutex::new(driver),
    });
    let (task, join) = JoinHandle::new(runtime.clone(), future);
    let poller = Arc::new(PollDuringWake {
        join: Mutex::new(join),
        helper: Mutex::new(None),
        result: Mutex::new(None),
    });
    let waker = Waker::from(poller.clone());
    let mut cx = Context::from_waker(&waker);
    assert!(Pin::new(&mut *poller.join.lock().unwrap())
        .poll(&mut cx)
        .is_pending());
    let task = if abort {
        task.cancel();
        // Release the queue lock before running cancellation and its callback.
        let task = runtime.global_queue.lock().unwrap().pop_front().unwrap();
        task
    } else {
        task
    };
    let run = catch_unwind(AssertUnwindSafe(|| task.run()));
    if let Some(helper) = poller.helper.lock().unwrap().take() {
        helper.join().unwrap();
    }
    assert!(
        run.is_ok(),
        "completion wake could not poll the join handle"
    );
    let result = poller.result.lock().unwrap().take().unwrap();
    result
}

#[test]
fn completion_wake_can_poll_successful_join() {
    assert_eq!(
        poll_during_completion_wake(std::future::ready(42), false).unwrap(),
        42
    );
}

#[test]
fn completion_wake_can_poll_panicked_join() {
    let future = std::future::poll_fn(|_| -> Poll<usize> { panic!("expected task panic") });
    assert!(matches!(
        poll_during_completion_wake(future, false),
        Err(JoinError::Panic(_))
    ));
}

#[test]
fn completion_wake_can_poll_cancelled_join() {
    assert!(matches!(
        poll_during_completion_wake(std::future::pending(), true),
        Err(JoinError::Aborted)
    ));
}

struct Notify(mpsc::Sender<()>);

impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        let _ = self.0.send(());
    }
}

// Force a real task to complete after JoinHandle's first output check but
// before AtomicWaker::register. No production hooks or timing assumptions.
struct CompleteOnRegister {
    inner: Arc<dyn DynTask>,
    abort: bool,
}

// SAFETY: get_link never returns, and scheduling delegates to the real task.
// This join-only adapter neither exposes nor moves the real task's future.
unsafe impl DynTask for CompleteOnRegister {
    fn register(&self, waker: &std::task::Waker) {
        if self.abort {
            self.inner.clone().cancel();
        }
        self.inner.clone().run();
        self.inner.register(waker);
    }

    fn take_output(&self, output: &mut dyn std::any::Any) {
        self.inner.take_output(output);
    }

    fn schedule_global(self: Arc<Self>) {
        self.inner.clone().schedule_global();
    }

    fn run(self: Arc<Self>) {
        self.inner.clone().run();
    }

    fn cancel(self: Arc<Self>) {
        self.inner.clone().cancel();
    }

    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<dyn DynTask>> {
        unreachable!("this join-only adapter queues the inner task directly")
    }
}

fn complete_during_registration<F>(future: F, abort: bool) -> Result<F::Output, JoinError>
where
    F: Future + Send + 'static,
    F::Output: Send,
{
    let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
    let (_resume_worker, resume) = mpsc::channel::<()>();
    if abort {
        let (entered, paused) = mpsc::channel();
        let _blocker = runtime.spawn(async move {
            entered.send(()).unwrap();
            let _ = resume.recv();
        });
        paused
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // cancel() queues cleanup as well as the direct run below. Keep the
        // worker paused so a second cleanup cannot mask the missed notification.
        // Dropping _resume_worker releases it on both success and panic.
    }
    let (task, mut join) = JoinHandle::new(runtime, future);
    join.task = Arc::new(CompleteOnRegister { inner: task, abort });
    let mut join = std::pin::pin!(join);

    let (tx, rx) = mpsc::channel();
    let waker = std::task::Waker::from(Arc::new(Notify(tx)));
    let mut cx = Context::from_waker(&waker);
    let mut result = join.as_mut().poll(&mut cx);
    if result.is_pending() {
        // A correct implementation may return Pending only if it arranged
        // another poll. The controlled completion has already finished.
        rx.try_recv()
            .expect("completed join returned Pending without a notification");
        result = join.as_mut().poll(&mut cx);
    }
    match result {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("completed task has no join result"),
    }
}

#[test]
fn result_between_output_check_and_registration_is_not_lost() {
    assert_eq!(
        complete_during_registration(async { 42 }, false).unwrap(),
        42
    );
}

#[test]
fn panic_between_output_check_and_registration_is_not_lost() {
    let result = complete_during_registration(async { panic!("task panic") }, false);
    assert!(matches!(result, Err(JoinError::Panic(_))));
}

#[test]
fn cancellation_between_output_check_and_registration_is_not_lost() {
    let result = complete_during_registration(std::future::pending::<()>(), true);
    assert!(matches!(result, Err(JoinError::Aborted)));
}

#[test]
fn completion_after_registration_wakes_the_joiner() {
    let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
    let (task, mut join) = JoinHandle::new(runtime, async { 42 });
    let (tx, rx) = mpsc::channel();
    let waker = std::task::Waker::from(Arc::new(Notify(tx)));
    let mut cx = Context::from_waker(&waker);

    assert!(Pin::new(&mut join).poll(&mut cx).is_pending());
    task.run();
    rx.try_recv()
        .expect("completion did not wake the registered joiner");
    assert!(matches!(
        Pin::new(&mut join).poll(&mut cx),
        Poll::Ready(Ok(42))
    ));
}
