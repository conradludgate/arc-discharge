use std::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use crate::task::{DynTask, Task};

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
    task: Arc<dyn DynTask>,
    output: PhantomData<O>,
}

impl<O> JoinHandle<O>
where
    O: Send + 'static,
{
    pub(crate) fn new(
        task: Task<impl Future<Output = O> + Send + 'static>,
    ) -> (Arc<dyn DynTask>, Self) {
        let task: Arc<dyn DynTask> = Arc::new(task);
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
        let mut out = Poll::Pending;

        #[cfg(debug_assertions)]
        {
            assert_eq!(self.task.output_type_id(), std::any::TypeId::of::<O>());
        }

        // a bit annoying we need unsafe here, but we need to type-erase the output.
        // a simple solution would be to allocate an arc<mutex<_>> and wrap our future
        // to write the output into that, but I want to avoid that alloc.
        // this means we are forced to use type erasure

        // SAFETY:
        // we know the task was created as a `JoinInner<Future<Output = ()>>` as
        // enforced by the `JoinHandle::new()` function.
        unsafe { self.task.take_output(&mut out as *mut _ as *mut ()) }

        if out.is_pending() {
            self.task.register(cx.waker());
        }
        out
    }
}
