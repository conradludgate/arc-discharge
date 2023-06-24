use std::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use arc_dyn::ThinArc;

use crate::{task::Task, PinTask};

pin_project_lite::pin_project!(
    #[project = JoinInnerProj]
    pub(crate) enum JoinInner<F>
    where
        F: Future,
    {
        Future {
            #[pin]
            fut: F,
        },
        Return {
            val: Option<F::Output>,
        },
    }
);

impl<F: Future> JoinInner<F> {
    pub(crate) fn new(fut: F) -> Self {
        Self::Future { fut }
    }
}

impl<F: Future> Future for JoinInner<F> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let x = match self.as_mut().project() {
            JoinInnerProj::Future { fut } => std::task::ready!(fut.poll(cx)),
            JoinInnerProj::Return { .. } => return Poll::Pending,
        };
        self.set(JoinInner::Return { val: Some(x) });
        Poll::Ready(())
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

impl<O: 'static> Future for JoinHandle<O> {
    type Output = O;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut fut = Task::lock_fut(&self.task);

        let mut out = Poll::Pending;

        #[cfg(debug_assertions)]
        {
            assert_eq!(fut.output_type_id(), std::any::TypeId::of::<O>());
        }

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

    #[cfg(debug_assertions)]
    fn output_type_id(&self) -> std::any::TypeId;
}

impl<F> FutureWithOutput for JoinInner<F>
where
    F: Future + Send + Sync + 'static,
    F::Output: Send + Sync + 'static,
{
    unsafe fn take_output(self: Pin<&mut Self>, output: *mut ()) {
        let output = output as *mut Poll<F::Output>;
        if let JoinInnerProj::Return { val } = self.project() {
            if let Some(val) = val.take() {
                // SAFETY: enforced by the caller of this unsafe fn that `output` is `Poll<Output>` and is writeable
                unsafe { output.write(Poll::Ready(val)) }
            }
        }
    }

    #[cfg(debug_assertions)]
    fn output_type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<F::Output>()
    }
}
