use futures_util::ready;
use mio::event::Source;
use mio::Interest;
use std::io;
use std::sync::Arc;
use std::task::{Context, Poll};

use super::{Direction, ReadyEvent};

/// Associates an I/O resource with the reactor instance that drives it.
///
/// A registration represents an I/O resource registered with a Reactor such
/// that it will receive task notifications on readiness. This is the lowest
/// level API for integrating with a reactor.
///
/// The association between an I/O resource is made by calling
/// [`new_with_interest_and_handle`].
/// Once the association is established, it remains established until the
/// registration instance is dropped.
///
/// A registration instance represents two separate readiness streams. One
/// for the read readiness and one for write readiness. These streams are
/// independent and can be consumed from separate tasks.
///
/// **Note**: while `Registration` is `Sync`, the caller must ensure that
/// there are at most two tasks that use a registration instance
/// concurrently. One task for [`poll_read_ready`] and one task for
/// [`poll_write_ready`]. While violating this requirement is "safe" from a
/// Rust memory safety point of view, it will result in unexpected behavior
/// in the form of lost notifications and tasks hanging.
///
/// ## Platform-specific events
///
/// `Registration` also allows receiving platform-specific `mio::Ready`
/// events. These events are included as part of the read readiness event
/// stream. The write readiness event stream is only for `Ready::writable()`
/// events.
///
/// [`new_with_interest_and_handle`]: method@Self::new_with_interest_and_handle
/// [`poll_read_ready`]: method@Self::poll_read_ready`
/// [`poll_write_ready`]: method@Self::poll_write_ready`
pub(crate) struct Registration {
    /// Index to state stored by the driver.
    key: usize,

    /// Handle to the associated runtime.
    handle: Arc<super::Handle>,
}

// ===== impl Registration =====

impl Registration {
    /// Registers the I/O resource with the reactor for the provided handle, for
    /// a specific `Interest`. This does not add `hup` or `error` so if you are
    /// interested in those states, you will need to add them to the readiness
    /// state passed to this function.
    ///
    /// # Return
    ///
    /// - `Ok` if the registration happened successfully
    /// - `Err` if an error was encountered during registration
    #[track_caller]
    pub(crate) fn new_with_interest_and_handle(
        io: &mut impl Source,
        interest: Interest,
        handle: Arc<super::Handle>,
    ) -> io::Result<Registration> {
        let key = handle.add_source(io, interest)?;

        Ok(Registration { handle, key })
    }

    /// Deregisters the I/O resource from the reactor it is associated with.
    ///
    /// This function must be called before the I/O resource associated with the
    /// registration is dropped.
    ///
    /// Note that deregistering does not guarantee that the I/O resource can be
    /// registered with a different reactor. Some I/O resource types can only be
    /// associated with a single reactor instance for their lifetime.
    ///
    /// # Return
    ///
    /// If the deregistration was successful, `Ok` is returned. Any calls to
    /// `Reactor::turn` that happen after a successful call to `deregister` will
    /// no longer result in notifications getting sent for this registration.
    ///
    /// `Err` is returned if an error is encountered.
    pub(crate) fn deregister(&mut self, io: &mut impl Source) -> io::Result<()> {
        self.handle.deregister_source(io)
    }

    pub(crate) fn clear_readiness(&self, event: ReadyEvent) {
        self.handle
            .slab
            .get(self.key)
            .unwrap()
            .clear_readiness(event);
    }

    // Uses the poll path, requiring the caller to ensure mutual exclusion for
    // correctness. Only the last task to call this function is notified.
    pub(crate) fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<ReadyEvent>> {
        self.poll_ready(cx, Direction::Read)
    }

    // Uses the poll path, requiring the caller to ensure mutual exclusion for
    // correctness. Only the last task to call this function is notified.
    pub(crate) fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<ReadyEvent>> {
        self.poll_ready(cx, Direction::Write)
    }

    // Uses the poll path, requiring the caller to ensure mutual exclusion for
    // correctness. Only the last task to call this function is notified.
    pub(crate) fn poll_read_io<R>(
        &self,
        cx: &mut Context<'_>,
        f: impl FnMut() -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        self.poll_io(cx, Direction::Read, f)
    }

    // Uses the poll path, requiring the caller to ensure mutual exclusion for
    // correctness. Only the last task to call this function is notified.
    pub(crate) fn poll_write_io<R>(
        &self,
        cx: &mut Context<'_>,
        f: impl FnMut() -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        self.poll_io(cx, Direction::Write, f)
    }

    /// Polls for events on the I/O resource's `direction` readiness stream.
    ///
    /// If called with a task context, notify the task when a new event is
    /// received.
    fn poll_ready(
        &self,
        cx: &mut Context<'_>,
        direction: Direction,
    ) -> Poll<io::Result<ReadyEvent>> {
        let ev = ready!(self
            .handle
            .slab
            .get(self.key)
            .unwrap()
            .poll_readiness(cx, direction));

        if ev.is_shutdown {
            return Poll::Ready(Err(gone()));
        }

        Poll::Ready(Ok(ev))
    }

    fn poll_io<R>(
        &self,
        cx: &mut Context<'_>,
        direction: Direction,
        mut f: impl FnMut() -> io::Result<R>,
    ) -> Poll<io::Result<R>> {
        loop {
            let ev = ready!(self.poll_ready(cx, direction))?;

            match f() {
                Ok(ret) => {
                    return Poll::Ready(Ok(ret));
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.clear_readiness(ev);
                }
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }

    pub(crate) fn try_io<R>(
        &self,
        interest: Interest,
        f: impl FnOnce() -> io::Result<R>,
    ) -> io::Result<R> {
        let ev = self
            .handle
            .slab
            .get(self.key)
            .unwrap()
            .ready_event(interest);

        // Don't attempt the operation if the resource is not ready.
        if ev.ready.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        match f() {
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.clear_readiness(ev);
                Err(io::ErrorKind::WouldBlock.into())
            }
            res => res,
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.handle.slab.clear(self.key);

        // // It is possible for a cycle to be created between wakers stored in
        // // `ScheduledIo` instances and `Arc<driver::Inner>`. To break this
        // // cycle, wakers are cleared. This is an imperfect solution as it is
        // // possible to store a `Registration` in a waker. In this case, the
        // // cycle would remain.
        // //
        // // See tokio-rs/tokio#3481 for more details.
        // self.shared.clear_wakers();
    }
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::Other, "error")
}

impl Registration {
    pub(crate) async fn readiness(&self, interest: Interest) -> io::Result<ReadyEvent> {
        let ev = super::Readiness {
            handle: &self.handle,
            key: self.key,
            state: super::State::Init,
            interest,
            waiter: pin_list::Node::new(),
        }
        .await;

        if ev.is_shutdown {
            return Err(gone());
        }

        Ok(ev)
    }

    pub(crate) async fn async_io<R>(
        &self,
        interest: Interest,
        mut f: impl FnMut() -> io::Result<R>,
    ) -> io::Result<R> {
        loop {
            let event = self.readiness(interest).await?;

            match f() {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.clear_readiness(event);
                }
                x => return x,
            }
        }
    }
}
