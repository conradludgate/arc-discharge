//! Type-erased task links over intrusive-collections.
//!
//! Each queued link owns one Arc strong reference, transferred by PointerOps.
//! Task construction binds the inverse callback to its exact enclosing type;
//! that callback recovers both allocation address and concrete dyn metadata.
//! LinkOps delegates membership acquisition and release to AtomicLinkOps, so
//! rejecting a duplicate must leave the existing owner's link intact.
//!
//! List membership is not the future's pinning guarantee: a dequeued task may
//! still be pending. Task's private construction/access boundary keeps futures
//! immovable through unlinking, polling and in-place completion/cancellation.

use std::{
    marker::PhantomData,
    mem::offset_of,
    ptr::{addr_of, NonNull},
    sync::Arc,
};

use intrusive_collections::{
    linked_list::{AtomicLink, LinkedListOps},
    DefaultLinkOps, DefaultPointerOps,
};

use crate::task::DynTask;

pub(crate) struct FatLink<D: ?Sized> {
    link: AtomicLink,
    get_value: unsafe fn(link: NonNull<FatLink<D>>) -> *const D,
}

impl<D: ?Sized> FatLink<D> {
    /// Creates an unlinked link with the inverse projection for `L`.
    ///
    /// # Safety
    /// This must be the link returned by `L::get_link` for its enclosing value.
    /// Using another type's callback can reconstruct an invalid pointer/vtable.
    pub(crate) const unsafe fn new<L: FatLinked<D>>() -> Self {
        FatLink {
            link: AtomicLink::new(),
            get_value: L::get_value,
        }
    }

    /// Checks whether the `AtomicLink` is linked into a `LinkedList`.
    #[inline]
    pub fn is_linked(&self) -> bool {
        self.link.is_linked()
    }

    #[inline]
    // ptr must identify a live FatLink, retaining provenance for its field.
    unsafe fn to_link(ptr: NonNull<Self>) -> NonNull<AtomicLink> {
        let offset = offset_of!(FatLink::<D>, link);
        unsafe { NonNull::new_unchecked(ptr.as_ptr().byte_add(offset).cast()) }
    }

    #[inline]
    // link must be the AtomicLink field of a live FatLink of this same type.
    unsafe fn from_link(link: NonNull<AtomicLink>) -> NonNull<Self> {
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

// SAFETY: Both projections refer to the same embedded AtomicLink. Membership
// acquisition/release and synchronization are delegated without changing them.
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

// SAFETY: Every list pointer is converted through the same inverse projections;
// the underlying AtomicLinkOps owns the next/prev and membership invariants.
unsafe impl<D: ?Sized> LinkedListOps for FatLinkOps<D> {
    #[inline]
    unsafe fn next(&self, ptr: Self::LinkPtr) -> Option<Self::LinkPtr> {
        self.ops
            .next(FatLink::to_link(ptr))
            .map(|link| FatLink::from_link(link))
    }

    #[inline]
    unsafe fn prev(&self, ptr: Self::LinkPtr) -> Option<Self::LinkPtr> {
        self.ops
            .prev(FatLink::to_link(ptr))
            .map(|link| FatLink::from_link(link))
    }

    unsafe fn set_next(&mut self, ptr: Self::LinkPtr, next: Option<Self::LinkPtr>) {
        self.ops.set_next(
            FatLink::to_link(ptr),
            next.map(|link| FatLink::to_link(link)),
        )
    }

    unsafe fn set_prev(&mut self, ptr: Self::LinkPtr, prev: Option<Self::LinkPtr>) {
        self.ops.set_prev(
            FatLink::to_link(ptr),
            prev.map(|link| FatLink::to_link(link)),
        )
    }
}

/// Inverse raw projections between a concrete allocation and its intrusive link.
///
/// # Safety
/// The projections must round-trip the same live allocation, with metadata for
/// its actual concrete type. The link and callback must remain valid and at the
/// same address while linked. Neither projection may move or release the owner.
pub(crate) unsafe trait FatLinked<D: ?Sized> {
    /// # Safety
    /// `self` must point to a live `Self` value.
    unsafe fn get_link(self: *const Self) -> NonNull<FatLink<D>>;
    /// # Safety
    /// `ptr` must be the link obtained from a live `Self` via `get_link`.
    unsafe fn get_value(ptr: NonNull<FatLink<D>>) -> *const D;
}

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

// SAFETY: DynTask requires a valid inverse callback for each returned link.
// DefaultPointerOps transfers one Arc strong reference into the collection and
// reconstructs it on removal/drop. No new reference count is invented here.
unsafe impl intrusive_collections::Adapter for TaskAdapter {
    type LinkOps = FatLinkOps<dyn DynTask>;
    type PointerOps = DefaultPointerOps<Arc<dyn DynTask>>;
    #[inline]
    unsafe fn get_value(&self, link: NonNull<FatLink<dyn DynTask>>) -> *const dyn DynTask {
        let get_value = *addr_of!((*link.as_ptr()).get_value);
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

#[cfg(test)]
mod tests;
