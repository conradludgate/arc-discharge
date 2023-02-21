use std::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use arc_dyn::ThinArc;

use crate::{task::Task, PinTask};

pin_project_lite::pin_project!(
    pub(crate) struct JoinInner<F: ?Sized>
    where
        F: Future,
    {
        done: bool,
        out: Option<F::Output>,
        #[pin]
        fut: F,
    }
);

impl<F: Future> JoinInner<F> {
    pub(crate) fn new(fut: F) -> Self {
        Self {
            done: false,
            out: None,
            fut,
        }
    }
}

impl<F: Future + ?Sized> Future for JoinInner<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if *this.done {
            Poll::Pending
        } else {
            match this.fut.poll(cx) {
                Poll::Ready(x) => {
                    *this.out = Some(x);
                    *this.done = true;
                    Poll::Ready(())
                }
                Poll::Pending => Poll::Pending,
            }
        }
    }
}

pub struct JoinHandle<O> {
    task: PinTask<dyn FutureWithOutput>,
    output: PhantomData<O>,
}

impl<O> JoinHandle<O>
where
    O: Send + Sync + 'static,
{
    pub(crate) fn new(
        task: Task<JoinInner<impl Future<Output = O> + Send + Sync + 'static>>,
    ) -> (PinTask<dyn FutureWithOutput>, Self) {
        let task: PinTask<dyn FutureWithOutput> = ThinArc::pin(task);
        let task2 = task.clone();

        let this = Self {
            task,
            output: PhantomData,
        };
        (task2, this)
    }
}

impl<O> Future for JoinHandle<O> {
    type Output = O;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut fut = Task::lock_fut(&self.task);

        let mut out = Poll::Pending;

        // a bit annoying we need unsafe here, but we need to type-erase the output.
        // a simple solution would be to allocate an arc<mutex<_>> and wrap our future
        // to write the output into that, but I want to avoid that alloc.
        // this means we are forced to use type erasure

        // SAFETY:
        // we know the task was created as a `JoinInner<Future<Output = ()>>` as
        // enforced by the `JoinHandle::new()` function.
        unsafe { fut.as_mut().take_output(&mut out as *mut _ as *mut ()) }

        if out.is_pending() {
            Task::register(&self.task, cx.waker());
        }
        out
    }
}

pub(crate) trait FutureWithOutput: Future<Output = ()> + Send + Sync + 'static {
    /// # Safety
    /// the output pointer must point to a valid `Poll<Output>` that is writeable and currently set to `Pending`
    unsafe fn take_output(self: Pin<&mut Self>, output: *mut ());
}

impl<F> FutureWithOutput for JoinInner<F>
where
    F: Future + Send + Sync + 'static,
    F::Output: Send + Sync + 'static,
{
    unsafe fn take_output(self: Pin<&mut Self>, output: *mut ()) {
        let output = output as *mut Poll<F::Output>;
        let this = self.project();
        if let Some(val) = this.out.take() {
            // SAFETY: enforced by the caller of this unsafe fn that `output` is `Poll<Output>` and is writeable
            unsafe { output.write(Poll::Ready(val)) }
        }
    }
}
