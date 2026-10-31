//! Early Heap-like structures for use before the heap is initialized.
//!
//! Be warned: every allocation has a footprint of at least `crate::arch::L1_PAGE_SIZE` bytes.

use core::alloc::Layout;

use alloc::alloc::Allocator;
use cake::log::error;

use crate::{
    ContextualizedError, MapFlags, MapSource, align,
    err::Contextualize,
    paging::{AddressExt, primitives::VirtRange},
};

/// A page backed allocator that does not require a heap to be initialized.
/// This is useful for early bootstrapping of the memory manager, where we need to allocate memory before the heap is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaplessAllocator(MapFlags);

impl HeaplessAllocator {
    /// Creates a new `HeaplessAllocator` with the given `MapFlags`.
    pub const fn new(flags: MapFlags) -> Self {
        Self(flags)
    }

    /// Allocates the given layout and returns a pointer to it if the allocation is successful.
    pub fn allocate(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<core::ptr::NonNull<[u8]>, ContextualizedError> {
        let layout = create_page_layout(layout);
        let vbase =
            crate::reserve_virtual(layout).with_ctx("failed to reserve backing virtual memory")?;

        if let Err(e) = crate::map(vbase, MapSource::Anon, self.0, None, &mut ()) {
            if let Err(e) = unsafe { crate::free_virtual(vbase, layout) } {
                error!(
                    "Failed to free virtual memory for heapless allocator: {:?}",
                    e
                );
            }
            return Err(e).with_ctx("failed to map underlying memory");
        }

        let ptr = vbase.start().as_mut_ptr::<u8>();
        let slice = core::ptr::slice_from_raw_parts_mut(ptr, layout.size());
        Ok(core::ptr::NonNull::new(slice).unwrap())
    }

    fn deallocate(
        &self,
        ptr: core::ptr::NonNull<u8>,
        layout: core::alloc::Layout,
    ) -> Result<(), ContextualizedError> {
        let layout = create_page_layout(layout);
        let vbase = crate::VirtAddr::from_ptr(ptr.as_ptr()).unwrap();
        // SAFETY: Upheld by the caller
        let range = VirtRange::new_len(vbase, layout.size() as u64);
        unsafe { crate::unmap(range) }.map_err(|e| {
            let _ = unsafe { crate::free_virtual(range, layout) };
            e.with_ctx("failed to unmap memory")
        })?;

        unsafe { crate::free_virtual(range, layout) }
            .map_err(|e| e.with_ctx("failed to free underlying virtual memory"))?;

        Ok(())
    }

    fn reallocate_fallback(
        &self,
        ptr: core::ptr::NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<core::ptr::NonNull<[u8]>, alloc::alloc::AllocError> {
        let new_ptr = Allocator::allocate(self, new_layout)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                ptr.as_ptr(),
                new_ptr.cast::<u8>().as_ptr(),
                old_layout.size(),
            );
            self.deallocate(ptr, old_layout);
        }
        Ok(new_ptr)
    }
}

unsafe impl Allocator for HeaplessAllocator {
    fn allocate(
        &self,
        layout: core::alloc::Layout,
    ) -> Result<core::ptr::NonNull<[u8]>, alloc::alloc::AllocError> {
        self.allocate(layout).map_err(|e| {
            error!("allocate failed: {}", e);
            alloc::alloc::AllocError
        })
    }

    unsafe fn deallocate(&self, ptr: core::ptr::NonNull<u8>, layout: core::alloc::Layout) {
        let _ = self
            .deallocate(ptr, layout)
            .inspect_err(|e| error!("deallocate failed: {}", e));
    }

    unsafe fn grow(
        &self,
        ptr: core::ptr::NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<core::ptr::NonNull<[u8]>, alloc::alloc::AllocError> {
        let old_pl = create_page_layout(old_layout);
        let new_pl = create_page_layout(new_layout);

        if new_pl.size() <= old_pl.size() {
            return Ok(core::ptr::NonNull::new(core::ptr::slice_from_raw_parts_mut(
                ptr.as_ptr(),
                new_pl.size(),
            ))
            .unwrap());
        }

        self.reallocate_fallback(ptr, old_layout, new_layout)
    }

    unsafe fn shrink(
        &self,
        ptr: core::ptr::NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<core::ptr::NonNull<[u8]>, alloc::alloc::AllocError> {
        let old_pl = create_page_layout(old_layout);
        let new_pl = create_page_layout(new_layout);

        if new_pl.size() >= old_pl.size() {
            return Ok(core::ptr::NonNull::new(core::ptr::slice_from_raw_parts_mut(
                ptr.as_ptr(),
                new_pl.size(),
            ))
            .unwrap());
        }

        self.reallocate_fallback(ptr, old_layout, new_layout)
    }
}

fn create_page_layout(layout: Layout) -> Layout {
    let size_pages = align!(up, layout.size(), crate::arch::L1_PAGE_SIZE as usize);
    let align_pages = usize::max(layout.align(), crate::arch::L1_PAGE_SIZE as usize);

    Layout::from_size_align(size_pages, align_pages).unwrap()
}
