use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{
            AtomicUsize,
            Ordering::{AcqRel, Acquire},
        },
        Mutex,
    },
    task::{Context, Poll},
    time::Duration,
};

use arrayvec::ArrayVec;
use pin_list::{id, PinList};

use ready::Ready;
use sharded_slab::{pool::Ref, Clear};

use crate::bits;

pub(crate) mod poll_evented;
pub(crate) mod ready;
mod registration;

struct SlabConfig;
impl sharded_slab::Config for SlabConfig {
    const MAX_THREADS: usize = sharded_slab::DefaultConfig::MAX_THREADS;
    const MAX_PAGES: usize = sharded_slab::DefaultConfig::MAX_PAGES;
    const INITIAL_PAGE_SIZE: usize = sharded_slab::DefaultConfig::INITIAL_PAGE_SIZE;
    const RESERVED_BITS: usize = 8;
}

pub struct IODriver {
    events: mio::Events,
    poll: mio::Poll,
    tick: u8,
}

pub(crate) struct Handle {
    registry: mio::Registry,
    slab: sharded_slab::Pool<ScheduledIo, SlabConfig>,
    waker: mio::Waker,
}

const WAKE_TOKEN: mio::Token = mio::Token(usize::MAX);

impl IODriver {
    pub(crate) fn new() -> (Self, Handle) {
        let driver = Self {
            events: mio::Events::with_capacity(1024),
            poll: mio::Poll::new().unwrap(),
            tick: 0,
        };
        let registry = driver.poll.registry();
        let handle = Handle {
            registry: registry.try_clone().unwrap(),
            slab: sharded_slab::Pool::new_with_config(),
            waker: mio::Waker::new(registry, WAKE_TOKEN).unwrap(),
        };
        (driver, handle)
    }

    pub(crate) fn poll_timeout(&mut self, handle: &Handle, timeout: Option<Duration>) {
        let events = &mut self.events;

        self.tick = self.tick.wrapping_add(1);

        // Block waiting for an event to happen, peeling out how many events
        // happened.
        match self.poll.poll(events, timeout) {
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => panic!("unexpected error when polling the I/O driver: {:?}", e),
        }

        for event in events.iter() {
            let token = event.token();

            match token {
                WAKE_TOKEN => {}
                mio::Token(index) => {
                    if let Some(waker) = handle.slab.get(index) {
                        let ready = Ready::from_mio(event);
                        waker.set_readiness(Tick::Set(self.tick), |curr| curr | ready);
                        waker.wake(ready)
                    }
                }
            }
        }
    }
}

type ScheduleIoSlot = Ref<'static, ScheduledIo, SlabConfig>;

impl Clear for ScheduledIo {
    fn clear(&mut self) {
        let state = *self.readiness.get_mut();

        let generation = GENERATION.unpack(state);
        let next = GENERATION.pack_lossy(generation + 1, 0);

        *self.readiness.get_mut() = next;
    }
}

impl Default for ScheduledIo {
    fn default() -> Self {
        ScheduledIo {
            readiness: AtomicUsize::new(0),
            waiters: Mutex::new(Waiters {
                list: PinList::new(pin_list::id::Checked::new()),
                writer: None,
                reader: None,
            }),
        }
    }
}

impl Handle {
    fn slab_entry(&self) -> Option<ScheduleIoSlot> {
        let entry = self.slab.create()?.downgrade();

        let entry =
            // SAFETY: the slab (in Arc<Self>) is guaranteed to live longer than the Ref
            unsafe {
                std::mem::transmute::<
                    Ref<'_, ScheduledIo, SlabConfig>,
                    Ref<'static, ScheduledIo, SlabConfig>,
                >(entry)
            };

        Some(entry)
    }

    /// Registers an I/O resource with the reactor for a given `mio::Ready` state.
    ///
    /// The registration token is returned.
    fn add_source(
        &self,
        source: &mut impl mio::event::Source,
        interest: mio::Interest,
    ) -> std::io::Result<ScheduleIoSlot> {
        const ADDRESS: bits::Pack =
            bits::Pack::least_significant(std::mem::size_of::<usize>() as u32 * 8 - 8);
        const GENERATION: bits::Pack = ADDRESS.then(7);

        let entry = self
            .slab_entry()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::OutOfMemory, "bruh"))?;

        let token = GENERATION.pack(entry.generation(), ADDRESS.pack(entry.key(), 0));

        self.registry
            .register(source, mio::Token(token), interest)?;

        Ok(entry)
    }

    /// Deregisters an I/O resource from the reactor.
    pub(super) fn deregister_source(
        &self,
        source: &mut impl mio::event::Source,
    ) -> std::io::Result<()> {
        self.registry.deregister(source)?;

        Ok(())
    }

    pub(super) fn wake(&self) {
        self.waker.wake().unwrap()
    }
}

type IORegisterTypes = dyn pin_list::Types<
    Id = id::Checked,
    Protected = std::task::Waker,
    Removed = (),
    Unprotected = mio::Interest,
>;

/// Stored in the I/O driver resource slab.
pub(crate) struct ScheduledIo {
    /// Packs the resource's readiness with the resource's generation.
    readiness: AtomicUsize,

    waiters: Mutex<Waiters>,
}

type WaitList = PinList<IORegisterTypes>;

struct Waiters {
    /// List of all current waiters.
    list: WaitList,
    /// Waker used for AsyncRead.
    reader: Option<std::task::Waker>,

    /// Waker used for AsyncWrite.
    writer: Option<std::task::Waker>,
}

impl ScheduledIo {
    /// Notifies all pending waiters that have registered interest in `ready`.
    ///
    /// There may be many waiters to notify. Waking the pending task **must** be
    /// done from outside of the lock otherwise there is a potential for a
    /// deadlock.
    ///
    /// A stack array of wakers is created and filled with wakers to notify, the
    /// lock is released, and the wakers are notified. Because there may be more
    /// than 32 wakers to notify, if the stack array fills up, the lock is
    /// released, the array is cleared, and the iteration continues.
    pub(super) fn wake(&self, ready: Ready) {
        let mut wakers = ArrayVec::<std::task::Waker, 32>::new();

        let mut waiters = self.waiters.lock().unwrap();

        // check for AsyncRead slot
        if ready.is_readable() {
            if let Some(waker) = waiters.reader.take() {
                wakers.push(waker);
            }
        }

        // check for AsyncWrite slot
        if ready.is_writable() {
            if let Some(waker) = waiters.writer.take() {
                wakers.push(waker);
            }
        }

        'outer: loop {
            let mut cursor = waiters.list.cursor_front_mut();

            while !wakers.is_full() {
                let check = cursor.unprotected().map(|x| ready.satisfies(*x));
                match check {
                    Some(false) => {}
                    Some(true) => {
                        let waker = cursor
                            .remove_current(())
                            .expect("we just checked that the item is protected");

                        wakers.push(waker)
                    }
                    None => break 'outer,
                }
            }

            drop(waiters);
            wakers.drain(..).for_each(std::task::Waker::wake);

            waiters = self.waiters.lock().unwrap();
        }

        drop(waiters);
        wakers.drain(..).for_each(std::task::Waker::wake);
    }
}

// The `ScheduledIo::readiness` (`AtomicUsize`) is packed full of goodness.
//
// | shutdown | generation |  driver tick | readiness |
// |----------+------------+--------------+-----------|
// |   1 bit  |   7 bits   +    8 bits    +   16 bits |

const READINESS: bits::Pack = bits::Pack::least_significant(16);

const TICK: bits::Pack = READINESS.then(8);

const GENERATION: bits::Pack = TICK.then(7);

const SHUTDOWN: bits::Pack = GENERATION.then(1);

pin_project_lite::pin_project!(
    /// Future returned by `readiness()`.
    struct Readiness<'a> {
        scheduled_io: &'a ScheduledIo,

        state: State,

        interest: mio::Interest,

        #[pin]
        waiter: pin_list::Node<IORegisterTypes>,
    }
);

#[derive(Debug)]
pub(crate) struct ReadyEvent {
    tick: u8,
    pub(crate) ready: Ready,
    is_shutdown: bool,
}

enum State {
    Init,
    Waiting,
    Done,
}

enum Tick {
    Set(u8),
    Clear(u8),
}
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum Direction {
    Read,
    Write,
}

impl ScheduledIo {
    pub(crate) fn generation(&self) -> usize {
        GENERATION.unpack(self.readiness.load(Acquire))
    }

    /// An async version of `poll_readiness` which uses a linked list of wakers.
    pub(crate) async fn readiness(&self, interest: mio::Interest) -> ReadyEvent {
        self.readiness_fut(interest).await
    }

    fn readiness_fut(&self, interest: mio::Interest) -> Readiness<'_> {
        Readiness {
            scheduled_io: self,
            state: State::Init,
            interest,
            waiter: pin_list::Node::new(),
        }
    }

    pub(crate) fn clear_readiness(&self, event: ReadyEvent) {
        // This consumes the current readiness state **except** for closed
        // states. Closed states are excluded because they are final states.
        let mask_no_closed = event.ready - Ready::READ_CLOSED - Ready::WRITE_CLOSED;

        self.set_readiness(Tick::Clear(event.tick), |curr| curr - mask_no_closed);
    }

    /// Sets the readiness on this `ScheduledIo` by invoking the given closure on
    /// the current value, returning the previous readiness value.
    ///
    /// # Arguments
    /// - `token`: the token for this `ScheduledIo`.
    /// - `tick`: whether setting the tick or trying to clear readiness for a
    ///    specific tick.
    /// - `f`: a closure returning a new readiness value given the previous
    ///   readiness.
    ///
    /// # Returns
    ///
    /// If the given token's generation no longer matches the `ScheduledIo`'s
    /// generation, then the corresponding IO resource has been removed and
    /// replaced with a new resource. In that case, this method returns `Err`.
    /// Otherwise, this returns the previous readiness.
    fn set_readiness(&self, tick: Tick, f: impl Fn(Ready) -> Ready) {
        let mut current = self.readiness.load(Acquire);

        loop {
            let current_generation = GENERATION.unpack(current);

            // Mask out the tick/generation bits so that the modifying
            // function doesn't see them.
            let current_readiness = Ready::from_usize(current);
            let new = f(current_readiness);

            let packed = match tick {
                Tick::Set(t) => TICK.pack(t as usize, new.as_usize()),
                Tick::Clear(t) => {
                    if TICK.unpack(current) as u8 != t {
                        // Trying to clear readiness with an old event!
                        return;
                    }

                    TICK.pack(t as usize, new.as_usize())
                }
            };

            let next = GENERATION.pack(current_generation, packed);

            match self
                .readiness
                .compare_exchange(current, next, AcqRel, Acquire)
            {
                Ok(_) => return,
                // we lost the race, retry!
                Err(actual) => current = actual,
            }
        }
    }
    pub(super) fn ready_event(&self, interest: mio::Interest) -> ReadyEvent {
        let curr = self.readiness.load(Acquire);

        ReadyEvent {
            tick: TICK.unpack(curr) as u8,
            ready: mask(interest) & Ready::from_usize(READINESS.unpack(curr)),
            is_shutdown: SHUTDOWN.unpack(curr) != 0,
        }
    }
    /// Polls for readiness events in a given direction.
    ///
    /// These are to support `AsyncRead` and `AsyncWrite` polling methods,
    /// which cannot use the `async fn` version. This uses reserved reader
    /// and writer slots.
    fn poll_readiness(&self, cx: &mut Context<'_>, direction: Direction) -> Poll<ReadyEvent> {
        let curr = self.readiness.load(Acquire);

        let ready = direction.mask() & Ready::from_usize(READINESS.unpack(curr));
        let is_shutdown = SHUTDOWN.unpack(curr) != 0;

        if ready.is_empty() && !is_shutdown {
            // Update the task info
            let mut waiters = self.waiters.lock().unwrap();
            let slot = match direction {
                Direction::Read => &mut waiters.reader,
                Direction::Write => &mut waiters.writer,
            };

            // Avoid cloning the waker if one is already stored that matches the
            // current task.
            match slot {
                Some(existing) if existing.will_wake(cx.waker()) => {}
                _ => *slot = Some(cx.waker().clone()),
            }

            // Try again, in case the readiness was changed while we were
            // taking the waiters lock
            let curr = self.readiness.load(Acquire);
            let ready = direction.mask() & Ready::from_usize(READINESS.unpack(curr));
            let is_shutdown = SHUTDOWN.unpack(curr) != 0;
            if is_shutdown {
                Poll::Ready(ReadyEvent {
                    tick: TICK.unpack(curr) as u8,
                    ready: direction.mask(),
                    is_shutdown,
                })
            } else if ready.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(ReadyEvent {
                    tick: TICK.unpack(curr) as u8,
                    ready,
                    is_shutdown,
                })
            }
        } else {
            Poll::Ready(ReadyEvent {
                tick: TICK.unpack(curr) as u8,
                ready,
                is_shutdown,
            })
        }
    }
}
fn mask(int: mio::Interest) -> Ready {
    match int {
        mio::Interest::READABLE => Ready::READABLE | Ready::READ_CLOSED,
        mio::Interest::WRITABLE => Ready::WRITABLE | Ready::WRITE_CLOSED,
        _ => Ready::EMPTY,
    }
}

impl Future for Readiness<'_> {
    type Output = ReadyEvent;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        use std::sync::atomic::Ordering::SeqCst;

        let mut this = self.project();

        loop {
            match *this.state {
                State::Init => {
                    // Optimistically check existing readiness
                    let curr = this.scheduled_io.readiness.load(SeqCst);
                    let ready = Ready::from_usize(READINESS.unpack(curr));
                    let is_shutdown = SHUTDOWN.unpack(curr) != 0;

                    let interest = *this.interest;
                    let ready = ready.intersection(interest);

                    if !ready.is_empty() || is_shutdown {
                        // Currently ready!
                        let tick = TICK.unpack(curr) as u8;
                        *this.state = State::Done;
                        return Poll::Ready(ReadyEvent {
                            tick,
                            ready,
                            is_shutdown,
                        });
                    }

                    // Wasn't ready, take the lock (and check again while locked).
                    let mut waiters = this.scheduled_io.waiters.lock().unwrap();

                    let curr = this.scheduled_io.readiness.load(SeqCst);
                    let mut ready = Ready::from_usize(READINESS.unpack(curr));
                    let is_shutdown = SHUTDOWN.unpack(curr) != 0;

                    if is_shutdown {
                        ready = Ready::ALL;
                    }

                    let ready = ready.intersection(interest);

                    if !ready.is_empty() || is_shutdown {
                        // Currently ready!
                        let tick = TICK.unpack(curr) as u8;
                        *this.state = State::Done;
                        return Poll::Ready(ReadyEvent {
                            tick,
                            ready,
                            is_shutdown,
                        });
                    }

                    waiters
                        .list
                        .push_front(this.waiter.as_mut(), cx.waker().clone(), interest);
                    *this.state = State::Waiting;
                }
                State::Waiting => {
                    // Currently in the "Waiting" state, implying the caller has
                    // a waiter stored in the waiter list (guarded by
                    // `notify.waiters`). In order to access the waker fields,
                    // we must hold the lock.

                    let mut waiters = this.scheduled_io.waiters.lock().unwrap();

                    if let Some(init) = this.waiter.as_mut().initialized_mut() {
                        if let Some(waker) = init.protected_mut(&mut waiters.list) {
                            // Update the waker, if necessary.
                            waker.clone_from(cx.waker());

                            return Poll::Pending;
                        }
                        init.take_removed(&waiters.list)
                            .expect("this should be in the list but unprotected");
                    }

                    // Our waker has been notified.
                    *this.state = State::Done;
                }
                State::Done => {
                    let curr = this.scheduled_io.readiness.load(Acquire);
                    let is_shutdown = SHUTDOWN.unpack(curr) != 0;

                    // The returned tick might be newer than the event
                    // which notified our waker. This is ok because the future
                    // still didn't return `Poll::Ready`.
                    let tick = TICK.unpack(curr) as u8;

                    // The readiness state could have been cleared in the meantime,
                    // but we allow the returned ready set to be empty.
                    let curr_ready = Ready::from_usize(READINESS.unpack(curr));
                    let ready = curr_ready.intersection(*this.interest);

                    return Poll::Ready(ReadyEvent {
                        tick,
                        ready,
                        is_shutdown,
                    });
                }
            }
        }
    }
}

impl Direction {
    pub(super) fn mask(self) -> Ready {
        match self {
            Direction::Read => Ready::READABLE | Ready::READ_CLOSED,
            Direction::Write => Ready::WRITABLE | Ready::WRITE_CLOSED,
        }
    }
}
