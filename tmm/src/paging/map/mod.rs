//! A trait for types that can map and unmap pages of a specific size. This is the main interface for mapping
//! and unmapping pages in the memory manager, and it abstracts over the architecture-specific details of how
//! page tables are manipulated to create mappings.

use core::fmt;

use arrayvec::ArrayVec;
use cake::log::trace;

use crate::{
    ContextualizedError, MapFlags, MemError,
    paging::{
        Address, FragmentManager, FragmentSize, Frame, Large, Medium, MemoryFragment, MemoryRange,
        Page, Small, VirtAddr,
        fragment::GreedyFragmentMapper,
        operation::OperationAllSizes,
        primitives::{AnyFragment, PageClass},
    },
};

mod provider;

pub use provider::{
    DataAllocator, DataWithTableAllocator, FullProvider, GlobalMemoryProvider, MemoryProvider,
    PhysLinear,
};

mod local;

pub(crate) use local::{LocalMemoryMapper, MapperMut};

/// A trait for types that can map and unmap pages of a specific size. This is the main interface for mapping
/// and unmapping pages in the memory manager, and it abstracts over the architecture-specific details of how
/// page tables are manipulated to create mappings.
pub trait SizedMemoryMapper<S: FragmentSize> {
    /// Maps the given page to the given frame with the specified flags,
    /// using the provided frame allocator to allocate any necessary intermediate page tables.
    ///
    /// - `dst`: The virtual page to map.
    /// - `src`: The physical frame to map to the virtual page.
    /// - `flags`: The mapping flags to use for the mapping.
    /// - `parent_table_flags`: Optional flags to use for any parent page tables that need to be allocated. If `None`, default flags will be used.
    ///                         This is ORed with the default flags, which as of now is only WRITABLE.
    /// - `allocator`: The frame allocator to use for allocating any necessary intermediate page tables.
    ///
    fn map_primitive<A>(
        &mut self,
        dst: Page<S>,
        src: Frame<S>,
        flags: MapFlags,
        parent_table_flags: Option<MapFlags>,
        allocator: &mut A,
    ) -> Result<Flush, MemError>
    where
        A: FragmentManager<Frame<Small>, Small>;

    /// Unmaps the given page, returning the frame that was mapped to it before, or an error if the page was not mapped.
    unsafe fn unmap_primitive(&mut self, page: Page<S>) -> Result<Unmapped<S>, MemError>;
}

/// A memory mapper that can map and unmap pages of any size.
pub trait MemoryMapper:
    SizedMemoryMapper<Small> + SizedMemoryMapper<Medium> + SizedMemoryMapper<Large>
{
    /// Maps a range of virtual addresses to physical frames, using the provided frame allocator for any necessary allocations of page tables.
    unsafe fn map_from<P: FullProvider>(
        &mut self,
        range: MemoryRange<VirtAddr>,
        flags: MapFlags,
        parent_table_flags: Option<MapFlags>,
        provider: &mut P,
        operation: &mut impl OperationAllSizes,
    ) -> Result<(), MemError> {
        trace!(
            "Mapping from base address {:x?} with length {:?} and flags {:?}",
            range.start().as_u64(),
            range.size(),
            flags
        );

        let mapper = GreedyFragmentMapper::<PageClass>::new(range.start(), range.size());
        for frag in mapper {
            match frag {
                AnyFragment::Small(prim) => {
                    let frame = provider.allocate_data()?;
                    unsafe { operation.execute(prim, frame)? };
                    self.map_primitive(
                        prim,
                        frame,
                        flags,
                        parent_table_flags,
                        &mut provider.table_allocator(),
                    )?
                    .flush();
                }
                AnyFragment::Medium(prim) => {
                    let frame = provider.allocate_data()?;
                    unsafe { operation.execute(prim, frame)? };
                    self.map_primitive(
                        prim,
                        frame,
                        flags,
                        parent_table_flags,
                        &mut provider.table_allocator(),
                    )?
                    .flush();
                }
                AnyFragment::Large(prim) => {
                    let frame = provider.allocate_data()?;
                    unsafe { operation.execute(prim, frame)? };
                    self.map_primitive(
                        prim,
                        frame,
                        flags,
                        parent_table_flags,
                        &mut provider.table_allocator(),
                    )?
                    .flush();
                }
            }
        }

        Ok(())
    }
}

/// A structure representing an unmapped page.
#[derive(Debug)]
pub struct Unmapped<S: FragmentSize> {
    /// The physical frame that was previously mapped to the page.
    pub mut(crate) frame: Frame<S>,
    /// Free parent page tables that were used to map the page, if any.
    ///
    /// Entries are only present when the table is empty, and the capacity is 4 for future 5 level paging support.
    pub mut(crate) parent_tables: ArrayVec<Frame<Small>, 4>,
    flush: Option<Flush>,
    /// The mapping flags that were used for the mapping before it was unmapped.
    pub mut(crate) flags: MapFlags,
}

impl<S: FragmentSize> Unmapped<S> {
    /// Creates a new `Unmapped` structure with the given frame, flush operation, and mapping flags.
    pub fn new(
        frame: Frame<S>,
        parent_tables: ArrayVec<Frame<Small>, 4>,
        flush: Option<Flush>,
        flags: MapFlags,
    ) -> Self {
        Self {
            frame,
            parent_tables,
            flush,
            flags,
        }
    }

    /// Flushes the TLB entry for the unmapped page, if it has not already been flushed.
    pub fn flush(&mut self) {
        self.flush.take().map(|f| f.flush());
    }
}

/// A wrapper type for a virtual address that needs to be flushed from the TLB after a mapping operation.
/// This is used to ensure that the TLB is properly flushed after unmapping pages, which is necessary to prevent stale mappings from being used.
#[must_use = "The returned `Flush` should be flushed after the mapping operation to ensure that there are no stale mappings."]
pub struct Flush(FlushInner);

impl Flush {
    /// Creates a new `Flush` that indicates that all TLB entries should be flushed. This is used when unmapping large pages, where multiple TLB entries may be affected.
    pub fn flush_all() -> Self {
        Self(FlushInner::FlushAll)
    }

    /// Creates a new `Flush` for the given virtual address.
    ///
    /// # Safety
    ///
    pub unsafe fn flush_page<S: FragmentSize>(page: Page<S>) -> Self {
        Self(FlushInner::Flush(page.start_address()))
    }

    /// Flushes the TLB entry for the virtual address contained in this `Flush`, or flushes all TLB entries if this `Flush` indicates that all entries should be flushed.
    pub fn flush(self) {
        self.0.flush();
    }

    /// Ignores this `Flush`.
    pub fn ignore(self) {}
}

enum FlushInner {
    Flush(VirtAddr),
    FlushAll,
}

impl FlushInner {
    pub fn flush(self) {
        match self {
            Self::Flush(addr) => unsafe { crate::arch::do_flush(addr) },
            Self::FlushAll => unsafe { crate::arch::do_flush_all() },
        }
    }
}

impl fmt::Debug for Flush {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            FlushInner::Flush(addr) => write!(f, "Flush({:?})", addr),
            FlushInner::FlushAll => write!(f, "FlushAll"),
        }
    }
}
