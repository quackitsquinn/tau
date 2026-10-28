//! Contains the core types and structures related to paging, such as page table entries, page tables, and the layout of the page table hierarchy. It also defines the virtual and physical address types used by the architecture.
pub mod accessor;
pub mod asm;
mod fragment;
pub mod heapless;
pub mod index;
pub(crate) mod limine;
pub mod map;
pub mod operation;
pub mod primitives;
pub mod recursive;
mod recursive_entry;
mod table;
pub mod translate;

pub(crate) use recursive_entry::RecursiveEntryManager;

pub use table::{PageTable, PageTableEntry};

use cake::log::trace;
pub use index::PageTableIndex;

use crate::{
    ContextualizedError, MapFlags, MapSource, MemError, MemoryMapping, ProvidedMemoryMapping,
    arch::{Mapper, PageEntryType},
    paging::{
        fragment::GreedyFragmentMapper,
        map::{
            DataAllocator, Flush, FullProvider, GlobalMemoryProvider, MemoryMapper, PhysLinear,
            SizedMemoryMapper, Unmapped,
        },
        operation::OperationAllSizes,
        primitives::{AnyFragment, PageClass, PrimitiveClass, VirtRange},
        recursive::RecursivePageTable,
    },
};

pub use primitives::Frame;
pub use primitives::MemoryRange;
pub use primitives::Page;
pub use primitives::{Address, AddressExt};
pub use primitives::{FragmentSize, Large, Medium, MemoryFragment, Small};
pub use primitives::{PhysAddr, VirtAddr};

/// The type used for page table entries in the current architecture.
pub type Table = [PageEntryType; crate::arch::ENTRY_COUNT];

/// A trait for managing ranges of memory primitives, such as pages. T
// I wish we didn't also have to specify the address space here..
pub unsafe trait FragmentManager<T: MemoryFragment<S>, S: FragmentSize> {
    /// Allocates a range of memory of the specified size and alignment, returning the starting address of the allocated range.
    fn allocate_fragment(&mut self) -> Result<T, MemError>;
    /// Deallocates a previously allocated range of memory, given the starting address and size of the range.
    fn deallocate_fragment(&mut self, primitive: T);
}

/// A trait for managing ranges of memory primitives of all sizes (small, medium, and large).
pub trait FullManager<C: PrimitiveClass>:
    FragmentManager<C::Fragment<Small>, Small>
    + FragmentManager<C::Fragment<Medium>, Medium>
    + FragmentManager<C::Fragment<Large>, Large>
{
    /// Allocates a small memory primitive (typically 4KB in size for x86_64 architecture).
    fn allocate_small(&mut self) -> Result<C::Fragment<Small>, MemError> {
        self.allocate_fragment()
    }

    /// Allocates a medium memory primitive (typically 2MB in size for x86_64 architecture).
    fn allocate_medium(&mut self) -> Result<C::Fragment<Medium>, MemError> {
        self.allocate_fragment()
    }

    /// Allocates a large memory primitive (typically 1GB in size for x86_64 architecture).
    fn allocate_large(&mut self) -> Result<C::Fragment<Large>, MemError> {
        self.allocate_fragment()
    }
}

/// Maps a memory primitive (such as a frame) to a page with the specified flags, using the provided frame allocator to allocate any necessary intermediate page tables.
#[must_use = "The returned `Flush` should be flushed after the mapping operation to ensure that there are no stale mappings."]
pub fn map_primitive<S, A>(
    src: Frame<S>,
    dst: Page<S>,
    flags: MapFlags,
    parent_table_flags: Option<MapFlags>,
    frame_allocator: &mut A,
) -> Result<Flush, MemError>
where
    S: FragmentSize,
    A: FragmentManager<Frame<Small>, Small>,
    Mapper: SizedMemoryMapper<S>,
{
    trace!(
        "Mapping frame {:?} to page {:?} with flags {}",
        src, dst, flags
    );

    let active_as = asm::active();
    let mut mapper = active_as.mapper().unwrap();

    mapper.map_primitive(dst, src, flags, parent_table_flags, frame_allocator)
}

/// Unmaps a page, returning the frame that was mapped to it before, or an error if the page was not mapped.
///
/// # Safety
///
/// The caller must ensure that there are no currently living references to the memory that was mapped to the page being unmapped,
/// as accessing that memory afterwards is undefined behavior.
#[must_use = "The returned `Flush` should be flushed after the mapping operation to ensure that there are no stale mappings."]
pub unsafe fn unmap_primitive<S>(dst: Page<S>) -> Result<Unmapped<S>, MemError>
where
    S: FragmentSize,
    RecursivePageTable<'static>: SizedMemoryMapper<S>,
{
    trace!("Unmapping page {:?}", dst);

    let active_as = asm::active();
    let mut mapper = active_as.mapper().unwrap();

    unsafe { mapper.unmap_primitive(dst) }
}

pub(crate) unsafe fn map_from<P>(
    range: MemoryRange<VirtAddr>,
    flags: MapFlags,
    parent_table_flags: Option<MapFlags>,
    provider: &mut P,
    operation: &mut impl OperationAllSizes,
) -> Result<(), MemError>
where
    P: FullProvider,
{
    trace!(
        "Mapping from base address {:x?} with length {:?} and flags {:?}",
        range.start().as_u64(),
        range.size(),
        flags
    );

    let active_as = asm::active();
    let mut mapper = active_as.mapper().unwrap();

    unsafe { mapper.map_from(range, flags, parent_table_flags, provider, operation) }
}

pub(crate) unsafe fn map_direct_unchecked(
    mapping: MemoryMapping,
    flags: MapFlags,
    parent_table_flags: Option<MapFlags>,
    op: &mut impl OperationAllSizes,
) -> Result<ProvidedMemoryMapping, MemError> {
    let range = mapping.virt_range();
    let phys_start = mapping.phys_range().start();

    trace!(
        "Mapping physical memory at address {:#x} to virtual address {:?} and flags {:?}",
        phys_start.as_u64(),
        range,
        flags
    );

    unsafe {
        map_from(
            range,
            flags,
            parent_table_flags,
            &mut PhysLinear(phys_start, &mut GlobalMemoryProvider),
            op,
        )?;
    }

    Ok(ProvidedMemoryMapping::from_mapping(mapping, false))
}

pub(crate) unsafe fn map_unchecked(
    dest: MemoryRange<VirtAddr>,
    src: MapSource,
    flags: MapFlags,
    parent_table_flags: Option<MapFlags>,
    op: &mut impl OperationAllSizes,
) -> Result<(), MemError> {
    match src {
        MapSource::Anon => unsafe {
            trace!(
                "Allocating to virtual address {:?} and flags {:?}",
                dest, flags
            );

            return map_from(
                dest,
                flags | MapFlags::DEALLOCATE,
                parent_table_flags,
                &mut DataAllocator(&mut GlobalMemoryProvider),
                op,
            );
        },
    }
}

pub(crate) unsafe fn unmap_unchecked(virt_range: VirtRange) -> Result<(), MemError> {
    let mapper = GreedyFragmentMapper::<PageClass>::new(virt_range.start(), virt_range.size());

    fn unmap<S: FragmentSize>(page: Page<S>) -> Result<(), MemError>
    where
        Mapper: SizedMemoryMapper<S>,
    {
        let mut ent = unsafe { unmap_primitive(page)? };
        ent.flush();
        let mut pmm = asm::pmm();
        if ent.flags.contains(MapFlags::DEALLOCATE) {
            pmm.deallocate_fragment(ent.frame);
        }

        for parent in ent.parent_tables.iter() {
            pmm.deallocate_fragment(*parent);
        }

        Ok(())
    }

    for frag in mapper {
        match frag {
            AnyFragment::Small(page_prim) => {
                unmap(page_prim)?;
            }
            AnyFragment::Medium(page_prim) => {
                unmap(page_prim)?;
            }
            AnyFragment::Large(page_prim) => {
                unmap(page_prim)?;
            }
        }
    }

    Ok(())
}
