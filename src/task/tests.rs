use super::*;
use std::{
    num::NonZeroUsize,
    sync::{atomic::AtomicUsize, mpsc},
    task::{Poll, Wake},
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn output_check_contention_does_not_discard_a_poll() {
    let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
    for _ in 0..32 {
        let task = Arc::new(Task::new(runtime.clone(), std::future::ready(7usize)));
        // take_output also locks this mutex while the future is still pending.
        // A worker can win IDLE -> RUNNING before that brief output check unlocks.
        let output_check = task.fut.lock();
        let running = task.clone();
        let worker = std::thread::spawn(move || running.run());
        let deadline = std::time::Instant::now() + TIMEOUT;
        while task.state.load(Ordering::Acquire) == IDLE {
            assert!(std::time::Instant::now() < deadline, "worker did not start");
            std::thread::yield_now();
        }
        drop(output_check);
        worker.join().unwrap();
        let mut result = Poll::<Result<usize, JoinError>>::Pending;
        task.take_output(&mut result);
        assert!(matches!(result, Poll::Ready(Ok(7))));
    }
}

struct PausedPoll {
    entered: Option<mpsc::Sender<Waker>>,
    release: mpsc::Receiver<()>,
    completed: mpsc::Sender<()>,
}
impl Future for PausedPoll {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(entered) = self.entered.take() {
            entered.send(cx.waker().clone()).unwrap();
            self.release.recv_timeout(TIMEOUT).unwrap();
            Poll::Pending
        } else {
            self.completed.send(()).unwrap();
            Poll::Ready(())
        }
    }
}

fn worker_barrier(runtime: &Arc<MTRuntime>) {
    let (tx, rx) = mpsc::channel();
    let _sentinel = runtime.spawn(async move { tx.send(()).unwrap() });
    rx.recv_timeout(TIMEOUT)
        .expect("worker did not reach sentinel");
}

#[test]
fn notification_consumed_during_poll_is_preserved() {
    for notifications in [1, 16] {
        let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let task = Arc::new(Task::new(
            runtime.clone(),
            PausedPoll {
                entered: Some(entered_tx),
                release: release_rx,
                completed: done_tx,
            },
        ));
        let running = task.clone();
        let thread = std::thread::spawn(move || running.run());
        let waker = entered_rx.recv_timeout(TIMEOUT).unwrap();
        for _ in 0..notifications {
            waker.wake_by_ref();
        }
        // A real worker must consume the notification before the first poll
        // returns Pending. The single worker processes this sentinel afterward.
        worker_barrier(&runtime);
        assert!(!task.link.is_linked());
        release_tx.send(()).unwrap();
        thread.join().unwrap();
        done_rx
            .recv_timeout(TIMEOUT)
            .expect("notification was lost");
        assert!(matches!(
            *task.fut.lock(),
            JoinInner::Return { val: Ok(()) }
        ));
    }
}

#[test]
fn self_wakes_and_cross_worker_notifications_make_progress() {
    for workers in [1, 4] {
        let runtime = MTRuntime::new(NonZeroUsize::new(workers).unwrap());
        let (tx, rx) = mpsc::channel();
        let mut polls = 0;
        let _join = runtime.spawn(std::future::poll_fn(move |cx| {
            polls += 1;
            if polls < 1000 {
                for _ in 0..8 {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            } else {
                tx.send(polls).unwrap();
                Poll::Ready(())
            }
        }));
        assert_eq!(rx.recv_timeout(TIMEOUT).unwrap(), 1000);
    }
}

struct Notify(mpsc::Sender<()>);
impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        let _ = self.0.send(());
    }
}

#[test]
fn cancellation_during_a_notified_poll_wakes_the_joiner() {
    let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, _done_rx) = mpsc::channel();
    let (join_tx, join_rx) = mpsc::channel();
    let task = Arc::new(Task::new(
        runtime.clone(),
        PausedPoll {
            entered: Some(entered_tx),
            release: release_rx,
            completed: done_tx,
        },
    ));
    task.register(&Waker::from(Arc::new(Notify(join_tx))));
    let running = task.clone();
    let thread = std::thread::spawn(move || running.run());
    entered_rx.recv_timeout(TIMEOUT).unwrap().wake();
    worker_barrier(&runtime);
    task.clone().cancel();
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    join_rx
        .recv_timeout(TIMEOUT)
        .expect("cancelled joiner was not woken");
    let mut result = Poll::<Result<(), JoinError>>::Pending;
    task.take_output(&mut result);
    assert!(matches!(result, Poll::Ready(Err(JoinError::Aborted))));
}

#[test]
fn cancellation_before_polling_wakes_the_joiner() {
    let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
    let (_task, mut join) =
        crate::JoinHandle::new(Task::new(runtime, std::future::pending::<()>()));
    let (tx, rx) = mpsc::channel();
    let waker = Waker::from(Arc::new(Notify(tx)));
    let mut cx = Context::from_waker(&waker);
    assert!(Pin::new(&mut join).poll(&mut cx).is_pending());
    join.abort();
    rx.recv_timeout(TIMEOUT)
        .expect("cancelled joiner was not woken");
    assert!(matches!(
        Pin::new(&mut join).poll(&mut cx),
        Poll::Ready(Err(JoinError::Aborted))
    ));
}

#[test]
fn completed_tasks_ignore_retained_wakes_and_cancellation() {
    let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
    let polls = Arc::new(AtomicUsize::new(0));
    let polled = polls.clone();
    let (tx, rx) = mpsc::channel();
    let task = Arc::new(Task::new(
        runtime.clone(),
        std::future::poll_fn(move |cx| {
            polled.fetch_add(1, Ordering::Relaxed);
            tx.send(cx.waker().clone()).unwrap();
            Poll::Ready(7)
        }),
    ));
    task.clone().run();
    let waker = rx.recv_timeout(TIMEOUT).unwrap();
    for _ in 0..16 {
        waker.wake_by_ref();
    }
    task.clone().cancel();
    worker_barrier(&runtime);
    assert_eq!(polls.load(Ordering::Relaxed), 1);
    let mut result = Poll::<Result<i32, JoinError>>::Pending;
    task.take_output(&mut result);
    assert!(matches!(result, Poll::Ready(Ok(7))));
}
