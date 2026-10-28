use super::*;
use crate::{linked_list::FatLink, MTRuntime};
use std::{num::NonZeroUsize, ptr::NonNull, sync::mpsc, task::Wake};

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
