use std::{
    mem::offset_of,
    ptr::{addr_of_mut, NonNull},
    sync::Arc,
};

use futures_util::Future;
use intrusive_collections::{
    xor_linked_list::XorLinkedListOps, DefaultPointerOps, XorLinkedListAtomicLink,
};

use crate::task::{DynTask, Task};

#[allow(explicit_outlives_requirements)]
#[derive(Clone, Copy)]
pub(crate) struct TaskAdapter {
    link_ops: FatLinkOps,
    pointer_ops: intrusive_collections::DefaultPointerOps<Arc<dyn DynTask>>,
}

impl Default for TaskAdapter {
    #[inline]
    fn default() -> Self {
        Self::NEW
    }
}

#[allow(dead_code)]
impl TaskAdapter {
    pub const NEW: Self = TaskAdapter {
        link_ops: FatLinkOps(
            <XorLinkedListAtomicLink as intrusive_collections::DefaultLinkOps>::NEW,
        ),
        pointer_ops: DefaultPointerOps::<Arc<dyn DynTask>>::new(),
    };
    #[inline]
    pub fn new() -> Self {
        Self::NEW
    }
}

pub(crate) struct FatLink {
    link: XorLinkedListAtomicLink,
    get_value: unsafe fn(link: NonNull<FatLink>) -> *const dyn DynTask,
}

impl FatLink {
    pub(crate) const fn new<F: Future>() -> Self
    where
        Task<F>: DynTask,
    {
        unsafe fn get_value<F: Future>(ptr: NonNull<FatLink>) -> *const dyn DynTask
        where
            Task<F>: DynTask,
        {
            let offset = offset_of!(Task::<F>, link);
            ptr.as_ptr().sub(offset).cast::<Task<F>>().cast_const() as _
        }

        FatLink {
            link: XorLinkedListAtomicLink::new(),
            get_value: get_value::<F>,
        }
    }

    fn to_link(ptr: NonNull<Self>) -> NonNull<XorLinkedListAtomicLink> {
        let offset = offset_of!(FatLink, link);
        unsafe { NonNull::new_unchecked(ptr.as_ptr().byte_add(offset).cast()) }
    }
    fn from_link(link: NonNull<XorLinkedListAtomicLink>) -> NonNull<Self> {
        let offset = offset_of!(FatLink, link);
        unsafe { NonNull::new_unchecked(link.as_ptr().byte_sub(offset).cast()) }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FatLinkOps(intrusive_collections::xor_linked_list::AtomicLinkOps);

unsafe impl intrusive_collections::LinkOps for FatLinkOps {
    type LinkPtr = NonNull<FatLink>;

    unsafe fn acquire_link(&mut self, ptr: Self::LinkPtr) -> bool {
        self.0.acquire_link(FatLink::to_link(ptr))
    }

    unsafe fn release_link(&mut self, ptr: Self::LinkPtr) {
        self.0.release_link(FatLink::to_link(ptr))
    }
}

unsafe impl XorLinkedListOps for FatLinkOps {
    unsafe fn next(
        &self,
        ptr: Self::LinkPtr,
        prev: Option<Self::LinkPtr>,
    ) -> Option<Self::LinkPtr> {
        self.0
            .next(FatLink::to_link(ptr), prev.map(FatLink::to_link))
            .map(FatLink::from_link)
    }

    unsafe fn prev(
        &self,
        ptr: Self::LinkPtr,
        next: Option<Self::LinkPtr>,
    ) -> Option<Self::LinkPtr> {
        self.0
            .prev(FatLink::to_link(ptr), next.map(FatLink::to_link))
            .map(FatLink::from_link)
    }

    unsafe fn set(
        &mut self,
        ptr: Self::LinkPtr,
        prev: Option<Self::LinkPtr>,
        next: Option<Self::LinkPtr>,
    ) {
        self.0.set(
            FatLink::to_link(ptr),
            prev.map(FatLink::to_link),
            next.map(FatLink::to_link),
        )
    }

    unsafe fn replace_next_or_prev(
        &mut self,
        ptr: Self::LinkPtr,
        old: Option<Self::LinkPtr>,
        new: Option<Self::LinkPtr>,
    ) {
        self.0.replace_next_or_prev(
            FatLink::to_link(ptr),
            old.map(FatLink::to_link),
            new.map(FatLink::to_link),
        )
    }
}

unsafe impl intrusive_collections::Adapter for TaskAdapter {
    type LinkOps = FatLinkOps;
    type PointerOps = DefaultPointerOps<Arc<dyn DynTask>>;
    #[inline]
    unsafe fn get_value(&self, link: NonNull<FatLink>) -> *const dyn DynTask {
        let get_value = *addr_of_mut!((*link.as_ptr()).get_value);
        get_value(link)
    }

    #[inline]
    unsafe fn get_link(&self, value: *const dyn DynTask) -> NonNull<FatLink> {
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
