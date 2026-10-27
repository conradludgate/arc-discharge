use super::*;
use std::{num::NonZeroUsize, sync::mpsc, task::Poll, time::Duration};

const TIMEOUT: Duration = Duration::from_secs(5);

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

#[test]
fn cancellation_accepts_the_notified_state() {
    let runtime = MTRuntime::new(NonZeroUsize::new(1).unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (done_tx, _done_rx) = mpsc::channel();
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
    entered_rx.recv_timeout(TIMEOUT).unwrap().wake();
    worker_barrier(&runtime);
    task.clone().cancel();
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    assert!(matches!(
        *task.fut.lock(),
        JoinInner::Return {
            val: Err(JoinError::Aborted)
        }
    ));
}
