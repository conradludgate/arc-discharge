use std::{
    marker::PhantomData,
    mem::offset_of,
    ptr::{addr_of, addr_of_mut, NonNull},
    sync::Arc,
};

use futures_util::Future;
use intrusive_collections::{
    xor_linked_list::XorLinkedListOps, DefaultPointerOps, XorLinkedListAtomicLink,
};

use crate::task::{DynTask, Task};

pub(crate) struct FatLink<D: ?Sized> {
    link: XorLinkedListAtomicLink,
    get_value: unsafe fn(link: NonNull<FatLink<D>>) -> *const D,
}

impl<D: ?Sized> FatLink<D> {
    pub(crate) const fn new<L: DynLink<D>>() -> Self {
        FatLink {
            link: XorLinkedListAtomicLink::new(),
            get_value: L::get_value,
        }
    }

    #[inline]
    fn to_link(ptr: NonNull<Self>) -> NonNull<XorLinkedListAtomicLink> {
        let offset = offset_of!(FatLink::<D>, link);
        unsafe { NonNull::new_unchecked(ptr.as_ptr().byte_add(offset).cast()) }
    }

    #[inline]
    fn from_link(link: NonNull<XorLinkedListAtomicLink>) -> NonNull<Self> {
        let offset = offset_of!(FatLink::<D>, link);
        unsafe { NonNull::new_unchecked(link.as_ptr().byte_sub(offset).cast()) }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FatLinkOps<D: ?Sized> {
    ops: intrusive_collections::xor_linked_list::AtomicLinkOps,
    d: PhantomData<D>,
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

unsafe impl<D: ?Sized> XorLinkedListOps for FatLinkOps<D> {
    #[inline]
    unsafe fn next(
        &self,
        ptr: Self::LinkPtr,
        prev: Option<Self::LinkPtr>,
    ) -> Option<Self::LinkPtr> {
        self.ops
            .next(FatLink::to_link(ptr), prev.map(FatLink::to_link))
            .map(FatLink::from_link)
    }

    #[inline]
    unsafe fn prev(
        &self,
        ptr: Self::LinkPtr,
        next: Option<Self::LinkPtr>,
    ) -> Option<Self::LinkPtr> {
        self.ops
            .prev(FatLink::to_link(ptr), next.map(FatLink::to_link))
            .map(FatLink::from_link)
    }

    #[inline]
    unsafe fn set(
        &mut self,
        ptr: Self::LinkPtr,
        prev: Option<Self::LinkPtr>,
        next: Option<Self::LinkPtr>,
    ) {
        self.ops.set(
            FatLink::to_link(ptr),
            prev.map(FatLink::to_link),
            next.map(FatLink::to_link),
        )
    }

    #[inline]
    unsafe fn replace_next_or_prev(
        &mut self,
        ptr: Self::LinkPtr,
        old: Option<Self::LinkPtr>,
        new: Option<Self::LinkPtr>,
    ) {
        self.ops.replace_next_or_prev(
            FatLink::to_link(ptr),
            old.map(FatLink::to_link),
            new.map(FatLink::to_link),
        )
    }
}

#[allow(explicit_outlives_requirements)]
#[derive(Clone, Copy)]
pub(crate) struct TaskAdapter<D: ?Sized> {
    link_ops: FatLinkOps<D>,
    pointer_ops: intrusive_collections::DefaultPointerOps<Arc<D>>,
}

impl<D: ?Sized> Default for TaskAdapter<D> {
    #[inline]
    fn default() -> Self {
        Self::NEW
    }
}

#[allow(dead_code)]
impl<D: ?Sized> TaskAdapter<D> {
    pub const NEW: Self = TaskAdapter {
        link_ops: FatLinkOps {
            ops: <XorLinkedListAtomicLink as intrusive_collections::DefaultLinkOps>::NEW,
            d: PhantomData,
        },
        pointer_ops: DefaultPointerOps::<Arc<D>>::new(),
    };
    #[inline]
    pub fn new() -> Self {
        Self::NEW
    }
}

unsafe impl intrusive_collections::Adapter for TaskAdapter<dyn DynTask> {
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

pub(crate) trait DynLink<D: ?Sized> {
    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<D>>;
    unsafe fn get_value(ptr: NonNull<FatLink<D>>) -> *const D;
}

impl<F: Future + Send + 'static> DynLink<dyn DynTask> for Task<F>
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
