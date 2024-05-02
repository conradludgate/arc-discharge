use std::{
    future::Future,
    marker::PhantomData,
    panic::{catch_unwind, AssertUnwindSafe},
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
            val: Result<F::Output, JoinError>,
        },
    }
);

pub enum JoinError {
    Aborted,
    Panic(Box<dyn std::any::Any + Send + 'static>),
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Aborted => write!(fmt, "task was aborted"),
            Self::Panic(_) => write!(fmt, "task panicked"),
        }
    }
}

impl std::fmt::Debug for JoinError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Aborted => write!(fmt, "JoinError::Aborted"),
            Self::Panic(_) => write!(fmt, "JoinError::Panic(...)"),
        }
    }
}

impl std::error::Error for JoinError {}

impl<F: Future> JoinInner<F> {
    pub(crate) fn new(fut: F) -> Self {
        Self::Future { fut }
    }
}

impl<F: Future> Future for JoinInner<F> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let val = match self.as_mut().project() {
            JoinInnerProj::Future { fut } => {
                let res = catch_unwind(AssertUnwindSafe(|| fut.poll(cx)));
                match res {
                    Ok(Poll::Pending) => return Poll::Pending,
                    Ok(Poll::Ready(x)) => Ok(x),
                    Err(panic) => Err(JoinError::Panic(panic)),
                }
            }
            JoinInnerProj::Return { .. } => return Poll::Pending,
        };
        self.set(JoinInner::Return { val });
        Poll::Ready(())
    }
}

pub(crate) trait Request {
    fn push(&mut self, val: &mut dyn std::any::Any);
}

impl<O: 'static> Request for Poll<Result<O, JoinError>> {
    fn push(&mut self, val: &mut dyn std::any::Any) {
        let output = val.downcast_mut().expect("task corrupted");
        *self = Poll::Ready(std::mem::replace(output, Err(JoinError::Aborted)));
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

    pub fn abort(&self) {
        self.task.cancel()
    }
}

impl<O: 'static> Future for JoinHandle<O> {
    type Output = Result<O, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut out = Poll::Pending;
        self.task.take_output(&mut out);

        if out.is_pending() {
            self.task.register(cx.waker());
        }
        out
    }
}
