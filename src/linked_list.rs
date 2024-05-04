use std::{
    marker::PhantomData,
    mem::offset_of,
    ptr::{addr_of, addr_of_mut, NonNull},
    sync::Arc,
};

use futures_util::Future;
use intrusive_collections::{
    linked_list::{AtomicLink, LinkedListOps},
    DefaultLinkOps, DefaultPointerOps,
};

use crate::task::{DynTask, Task};

pub(crate) struct FatLink<D: ?Sized> {
    link: AtomicLink,
    get_value: unsafe fn(link: NonNull<FatLink<D>>) -> *const D,
}

impl<D: ?Sized> FatLink<D> {
    pub(crate) const fn new<L: FatLinked<D>>() -> Self {
        FatLink {
            link: AtomicLink::new(),
            get_value: L::get_value,
        }
    }

    #[inline]
    fn to_link(ptr: NonNull<Self>) -> NonNull<AtomicLink> {
        let offset = offset_of!(FatLink::<D>, link);
        unsafe { NonNull::new_unchecked(ptr.as_ptr().byte_add(offset).cast()) }
    }

    #[inline]
    fn from_link(link: NonNull<AtomicLink>) -> NonNull<Self> {
        let offset = offset_of!(FatLink::<D>, link);
        unsafe { NonNull::new_unchecked(link.as_ptr().byte_sub(offset).cast()) }
    }
}

impl<D: ?Sized> DefaultLinkOps for FatLink<D> {
    type Ops = FatLinkOps<D>;

    const NEW: Self::Ops = FatLinkOps {
        ops: <AtomicLink as intrusive_collections::DefaultLinkOps>::NEW,
        d: PhantomData,
    };
}

pub(crate) struct FatLinkOps<D: ?Sized> {
    ops: intrusive_collections::linked_list::AtomicLinkOps,
    d: PhantomData<D>,
}

impl<D: ?Sized> Copy for FatLinkOps<D> {}
impl<D: ?Sized> Clone for FatLinkOps<D> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D: ?Sized> Default for FatLinkOps<D> {
    fn default() -> Self {
        <FatLink<D> as DefaultLinkOps>::NEW
    }
}

unsafe impl<D: ?Sized> intrusive_collections::LinkOps for FatLinkOps<D> {
    type LinkPtr = NonNull<FatLink<D>>;

    #[inline]
    unsafe fn acquire_link(&mut self, ptr: Self::LinkPtr) -> bool {
        self.ops.acquire_link(FatLink::to_link(ptr))
    }

    #[inline]
    unsafe fn release_link(&mut self, ptr: Self::LinkPtr) {
        self.ops.release_link(FatLink::to_link(ptr))
    }
}

unsafe impl<D: ?Sized> LinkedListOps for FatLinkOps<D> {
    #[inline]
    unsafe fn next(&self, ptr: Self::LinkPtr) -> Option<Self::LinkPtr> {
        self.ops.next(FatLink::to_link(ptr)).map(FatLink::from_link)
    }

    #[inline]
    unsafe fn prev(&self, ptr: Self::LinkPtr) -> Option<Self::LinkPtr> {
        self.ops.prev(FatLink::to_link(ptr)).map(FatLink::from_link)
    }

    unsafe fn set_next(&mut self, ptr: Self::LinkPtr, next: Option<Self::LinkPtr>) {
        self.ops
            .set_next(FatLink::to_link(ptr), next.map(FatLink::to_link))
    }

    unsafe fn set_prev(&mut self, ptr: Self::LinkPtr, prev: Option<Self::LinkPtr>) {
        self.ops
            .set_prev(FatLink::to_link(ptr), prev.map(FatLink::to_link))
    }
}

pub(crate) trait FatLinked<D: ?Sized> {
    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<D>>;
    unsafe fn get_value(ptr: NonNull<FatLink<D>>) -> *const D;
}

/// TODO: move the above to a separate crate, make the below a safe macro

#[allow(explicit_outlives_requirements)]
#[derive(Clone, Copy)]
pub(crate) struct TaskAdapter {
    link_ops: FatLinkOps<dyn DynTask>,
    pointer_ops: intrusive_collections::DefaultPointerOps<Arc<dyn DynTask>>,
}

impl Default for TaskAdapter {
    #[inline]
    fn default() -> Self {
        Self::NEW
    }
}

impl TaskAdapter {
    pub const NEW: Self = TaskAdapter {
        link_ops: <FatLink<dyn DynTask> as intrusive_collections::DefaultLinkOps>::NEW,
        pointer_ops: DefaultPointerOps::<Arc<dyn DynTask>>::new(),
    };
    #[inline]
    pub fn new() -> Self {
        Self::NEW
    }
}

unsafe impl intrusive_collections::Adapter for TaskAdapter {
    type LinkOps = FatLinkOps<dyn DynTask>;
    type PointerOps = DefaultPointerOps<Arc<dyn DynTask>>;
    #[inline]
    unsafe fn get_value(&self, link: NonNull<FatLink<dyn DynTask>>) -> *const dyn DynTask {
        let get_value = *addr_of_mut!((*link.as_ptr()).get_value);
        get_value(link)
    }

    #[inline]
    unsafe fn get_link(&self, value: *const dyn DynTask) -> NonNull<FatLink<dyn DynTask>> {
        value.get_link()
    }

    #[inline]
    fn link_ops(&self) -> &Self::LinkOps {
        &self.link_ops
    }
    #[inline]
    fn link_ops_mut(&mut self) -> &mut Self::LinkOps {
        &mut self.link_ops
    }
    #[inline]
    fn pointer_ops(&self) -> &Self::PointerOps {
        &self.pointer_ops
    }
}

impl<F: Future + Send + 'static> FatLinked<dyn DynTask> for Task<F>
where
    F::Output: Send + 'static,
{
    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<dyn DynTask>> {
        NonNull::new_unchecked(addr_of!((*self).link).cast_mut())
    }

    unsafe fn get_value(ptr: NonNull<FatLink<dyn DynTask>>) -> *const dyn DynTask {
        let offset = offset_of!(Task::<F>, link);
        ptr.as_ptr().sub(offset).cast::<Task<F>>().cast_const() as _
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, sync::Arc, task::Poll};

    use intrusive_collections::LinkedList;

    use crate::{
        task::{DynTask, Task},
        JoinError, MTRuntime,
    };

    use super::TaskAdapter;

    impl dyn DynTask {
        fn check<T: PartialEq + 'static>(self: Arc<Self>, t: T) {
            self.clone().run();
            let mut output = Poll::<Result<T, JoinError>>::Pending;
            self.take_output(&mut output);
            assert!(matches!(output, Poll::Ready(Ok(u)) if t == u))
        }
    }

    #[test]
    fn list() {
        let handle = MTRuntime::new(NonZeroUsize::new(1).unwrap());
        let mut list = LinkedList::new(TaskAdapter::new());
        list.push_back(Arc::new(Task::new(handle.clone(), async { 1 })) as Arc<_>);
        list.push_back(Arc::new(Task::new(handle.clone(), async { 2 })) as Arc<_>);
        list.push_back(Arc::new(Task::new(handle.clone(), async { 3 })) as Arc<_>);
        list.push_back(
            Arc::new(Task::new(handle.clone(), async { String::from("four") })) as Arc<_>,
        );

        list.pop_front().unwrap().check(1);
        list.pop_front().unwrap().check(2);
        list.pop_front().unwrap().check(3);
        list.pop_front().unwrap().check(String::from("four"));
    }
}
