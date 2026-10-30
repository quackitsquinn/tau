//! Memory allocator for the Tau kernel.
#![cfg_attr(not(test), no_std)]
#![feature(mut_restriction)]
#![feature(impl_restriction)]
#![feature(allocator_api)]
#![feature(gen_blocks)]

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::RefCell,
    cmp::Ordering,
    mem,
    ops::Range,
    ptr::NonNull,
    slice,
};

use alloc::{
    alloc::{AllocError, Allocator},
    vec::Vec,
};
use arrayvec::ArrayVec;
use tmm::{
    ContextualizedError, MapFlags, MapSource, MemError, align,
    corelocal::{CoreLocal, NoDefaultInit, no_default_init},
    paging::{AddressExt, heapless::HeaplessAllocator, primitives::VirtRange},
};

use crate::heap::Heap;

mod heap;

extern crate alloc;

/// A simple heap allocator that manages a contiguous range of virtual memory for dynamic allocation.
#[derive(Debug)]
pub struct HeapAllocator {
    heap: RefCell<Option<Heap>>,
    config: HeapConfig,
}

impl HeapAllocator {
    /// Creates a new `HeapAllocator` with the specified configuration.
    ///
    /// This does not initialize the heap; call `init` to set up the heap after creating the allocator.
    pub const fn new_const(heap_config: HeapConfig) -> Self {
        Self {
            heap: RefCell::new(None),
            config: heap_config,
        }
    }

    /// Creates a new `HeapAllocator` with the specified configuration and initializes the heap.
    ///
    /// This is a convenience method that combines creation and initialization.
    pub fn new(heap_config: HeapConfig) -> Result<Self, ContextualizedError> {
        let new = Self::new_const(heap_config);
        new.init()?;
        Ok(new)
    }

    /// Initializes the heap allocator by creating a new heap with the specified configuration.
    ///
    /// # Errors
    ///
    /// Returns a `ContextualizedError` if the heap has already been initialized or if the heap creation fails.
    pub fn init(&self) -> Result<(), ContextualizedError> {
        let mut heap_ref = self.heap.borrow_mut();
        if heap_ref.is_some() {
            return Err(MemError::InvalidOperation.with_ctx("already init"));
        }
        let heap = Heap::new(self.config)?;
        *heap_ref = Some(heap);
        Ok(())
    }
}

unsafe impl Allocator for HeapAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let mut heap_ref = self.heap.borrow_mut();
        let heap = heap_ref.as_mut().ok_or(AllocError)?;
        heap.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let mut heap_ref = self.heap.borrow_mut();
        let heap = heap_ref.as_mut().expect("Heap not initialized");
        unsafe { heap.deallocate(ptr, layout) };
    }
}

/// A global allocator that provides a heap allocator for each core in a multi-core system.
#[derive(Debug)]
pub struct GlobalAllocator {
    cfg: HeapConfig,
    allocators: CoreLocal<HeapAllocator, NoDefaultInit>,
}

impl GlobalAllocator {
    /// Creates a new `GlobalAllocator` with the specified heap configuration.
    pub const fn new(heap_config: HeapConfig) -> Self {
        Self {
            cfg: heap_config,
            allocators: CoreLocal::new(no_default_init()),
        }
    }

    /// Returns an `Option` containing a reference to the heap allocator for the current core, if it has been initialized.
    pub fn try_get(&self) -> Result<&HeapAllocator, ContextualizedError> {
        let mut err = None;
        self.allocators
            .get_with_inline(|_| match HeapAllocator::new(self.cfg) {
                Ok(allocator) => allocator,
                Err(e) => {
                    err = Some(e);
                    HeapAllocator::new_const(self.cfg) // Return a dummy allocator; it won't be used
                }
            });

        if let Some(e) = err {
            Err(e)
        } else {
            Ok(self.allocators.try_get().unwrap())
        }
    }

    /// Returns a reference to the heap allocator for the current core.
    pub fn get(&self) -> &HeapAllocator {
        self.allocators
            .get_with_inline(|cid| match HeapAllocator::new(self.cfg) {
                Ok(allocator) => allocator,
                Err(e) => panic!(
                    "Failed to initialize heap allocator for core {}: {:?} {}",
                    cid,
                    e.cause(),
                    e.trace()
                ),
            })
    }

    /// Initializes the heap allocator for the current core if it has not been initialized yet.
    pub fn init(&self) -> Result<bool, MemError> {
        if self.allocators.is_init() {
            Ok(false)
        } else {
            let _ = self.get();
            Ok(true)
        }
    }
}

unsafe impl GlobalAlloc for GlobalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match self.get().allocate(layout) {
            Ok(ptr) => ptr.as_ptr() as *mut u8,
            Err(_) => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let non_null_ptr = NonNull::new(ptr).expect("Pointer is null");
        unsafe { self.get().deallocate(non_null_ptr, layout) };
    }
}

/// Configuration for the heap allocator, specifying the initial and maximum heap sizes.
#[derive(Debug, Clone, Copy)]
pub struct HeapConfig {
    /// The initial size of the heap in bytes.
    pub initial_heap_size: usize,
    /// The maximum size of the heap in bytes.
    pub max_heap_size: usize,
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_find_insert_point() {
        let data = vec![1, 3, 5, 7, 9];
        assert_eq!(super::find_insert_point(&data, &0, |x| x), 0);
        assert_eq!(super::find_insert_point(&data, &1, |x| x), 0);
        assert_eq!(super::find_insert_point(&data, &2, |x| x), 1);
        assert_eq!(super::find_insert_point(&data, &3, |x| x), 1);
        assert_eq!(super::find_insert_point(&data, &4, |x| x), 2);
        assert_eq!(super::find_insert_point(&data, &5, |x| x), 2);
    }
}
