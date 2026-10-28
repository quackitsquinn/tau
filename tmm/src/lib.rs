//! nmm - Novos Memory Manager Library
#![cfg_attr(not(test), no_std)]
#![feature(ptr_alignment_type)]
#![feature(portable_simd)]
// const traits are incredibly useful, but they require a loot of nightly features to be enabled, so we enable them all here.
#![feature(const_trait_impl)]
#![feature(const_ops)]
#![feature(const_destruct)]
#![feature(const_cmp)]
#![feature(derive_const)]
#![feature(impl_restriction)]
#![feature(mut_restriction)]
#![feature(min_adt_const_params)]
#![feature(allocator_api)]

use core::{
    alloc::Layout,
    fmt::{Debug, Display},
};

extern crate alloc;

use arrayvec::ArrayVec;
use bitflags::bitflags;
use cake::{limine::memory_map, stacktrace::StackTrace};

#[doc(hidden)]
pub use pastey as _pastey;

use crate::{
    entry_walker::EntryWalker,
    paging::{
        Address, AddressExt, FragmentManager, FragmentSize, Frame, Large, MemoryFragment, Page,
        PageTableIndex, PhysAddr, VirtAddr,
        accessor::CopyMappingError,
        asm,
        operation::OperationAllSizes,
        primitives::{AnyFragment, MemoryRange, PageClass, PhysRange, VirtRange},
    },
};

#[cfg(not(feature = "x86_64"))]
compile_error!("Only x86_64 architecture is currently supported.");

pub mod arch;
pub mod bitmap;
pub mod corelocal;
pub mod entry_walker;
pub mod err;
mod init;
pub mod kernel_map;
pub mod paging;

pub use err::{ContextualizedError, MemError};
/// The minimum size of the managed range required for the memory manager to function correctly.
pub const MIN_MANAGED_RANGE_SIZE: u64 = arch::L3_PAGE_SIZE + arch::L2_PAGE_SIZE;

/// The configuration for initializing the memory manager.
#[derive(Clone, Copy)]
pub struct InitConfig {
    /// The recursive index used for the recursive page table mapping. This index is used to access the page tables in a
    /// recursive manner, allowing for efficient management of virtual memory.
    pub recursive_idx: PageTableIndex,
    /// A range of virtual memory that the memory manager will manage. This is used for virtual address allocation.
    /// This range is used for virtual address allocation (e.g., for `alloc_virtspace`) and physical memory mapping (e.g., for `alloc_paged`),
    /// as well as internal memory management state.
    pub managed_range: MemoryRange<VirtAddr>,
    /// A page that is reserved for zeroing out frames before they are returned to the caller (if requested).
    pub zero_page: Page<Large>,
    /// The physical memory map of the system, typically provided by the bootloader.
    ///
    // TODO: It seems reasonable to just make this a ArrayVec or something, since this is *super* clunky if you aren't using limine.
    pub memory_map: &'static [&'static memory_map::Entry],
}

impl Debug for InitConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InitConfig")
            .field("recursive_idx", &self.recursive_idx)
            .field("managed_range", &self.managed_range)
            .field("zero_page", &self.zero_page)
            .finish()
    }
}

impl InitConfig {
    /// Creates a new `InitConfig`, taking space out of the managed range for the scratch page.
    ///
    /// This means the managed range must be at least `arch::L3_PAGE_SIZE` bytes in size, and the scratch page will be the first page in the managed range.
    pub fn find_scratch_page(
        recursive_idx: PageTableIndex,
        managed_range: MemoryRange<VirtAddr>,
        memory_map: &'static [&'static memory_map::Entry],
    ) -> Result<Self, MemError> {
        if managed_range.size() < arch::L3_PAGE_SIZE {
            return Err(MemError::scratch_space_too_small(managed_range.size()));
        }

        let managed_aligned = managed_range
            .align_up_to::<Large>()
            .ok_or_else(|| MemError::scratch_space_too_small(managed_range.size()))?;

        if managed_aligned.start() + arch::L3_PAGE_SIZE > managed_range.end() {
            return Err(MemError::scratch_space_too_small(managed_range.size()));
        }

        let zero_page = Page::<Large>::from_start_address(managed_aligned.start()).unwrap();
        let managed_range = MemoryRange::new(
            managed_aligned.start() + arch::L3_PAGE_SIZE,
            managed_aligned.end(),
        );

        Ok(Self {
            recursive_idx,
            managed_range,
            zero_page,
            memory_map,
        })
    }
}

/// Initializes the memory manager.
///
/// - `root` is a pointer to the root page table structure, which will be used as the base for all page table operations. The specific type and
///          structure of this root page table is architecture-specific and will be defined in the architecture-specific implementation.
///          This allows the memory manager to work with different page table formats and structures depending on the architecture being used.
/// - `offset` is the virtual address offset where the physical memory is mapped in the virtual address space.
/// - `ranges` is a slice of memory map entries provided by the bootloader, describing the physical memory layout and available memory regions.
///    Currently, this is hardcoded to use the limine memory map, but in the future this could be converted to a more generic format to allow for different bootloaders
///    or custom memory map formats.
/// - `managed_range` is a tuple containing the starting virtual address and size of a range of virtual memory manager will manage.
///   This range is used for virtual address allocation (e.g., for `alloc_virtspace`) and physical memory mapping (e.g., for `alloc_paged`),
///   as well as internal memory management state.
pub unsafe fn init(init_config: InitConfig) -> Result<(), ContextualizedError> {
    unsafe { init::init_unchecked(EntryWalker::new(init_config.memory_map)?, init_config) }
}

/// A source of physical memory for mapping virtual addresses.
/// This enum defines the various ways in which physical memory can be allocated or copied when creating a new mapping in the virtual address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MapSource {
    /// Allocate physical memory for the mapping.
    Anon,
}

/// A structure representing a mapping between a virtual address range and a physical address range, along with the size of the mapping in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryMapping {
    /// The starting virtual address of the mapping.
    pub mut(crate) virt_start: VirtAddr,
    /// The starting physical address of the mapping.
    pub mut(crate) phys_start: PhysAddr,
    /// The size of the mapping in bytes.
    pub mut(crate) size: u64,
}

impl MemoryMapping {
    /// Creates a new `MemoryMapping` instance with the specified virtual base address, physical base address, and size in bytes.
    pub fn new(virt_start: VirtAddr, phys_start: PhysAddr, size: u64) -> Option<Self> {
        if virt_start.checked_add(size).is_none() || phys_start.checked_add(size).is_none() {
            return None;
        }

        Some(Self {
            virt_start,
            phys_start,
            size,
        })
    }

    /// Returns the virtual address range of the mapping.
    pub fn virt_range(&self) -> MemoryRange<VirtAddr> {
        MemoryRange::new(self.virt_start, self.virt_start + self.size)
    }

    /// Returns the physical address range of the mapping.
    pub fn phys_range(&self) -> MemoryRange<PhysAddr> {
        MemoryRange::new(self.phys_start, self.phys_start + self.size)
    }
}

/// Directly maps a physical address range to a virtual address range of the specified size with the given flags.
pub fn map_direct(
    mapping: MemoryMapping,
    flags: MapFlags,
    parent_table_flags: Option<MapFlags>,
    op: &mut impl OperationAllSizes,
) -> Result<ProvidedMemoryMapping, MemError> {
    unsafe { paging::map_direct_unchecked(mapping, flags, parent_table_flags, op) }
}

/// Maps the given source of physical memory to the specified virtual address range with the given flags.
pub fn map(
    dest: MemoryRange<VirtAddr>,
    src: MapSource,
    flags: MapFlags,
    parent_table_flags: Option<MapFlags>,
    op: &mut impl OperationAllSizes,
) -> Result<(), MemError> {
    unsafe { paging::map_unchecked(dest, src, flags, parent_table_flags, op) }
}

/// Unmaps a virtual address range of the specified size starting from the given virtual base address
///
/// - `virt_base` is the starting virtual address of the range to be unmapped.
/// - `byte_size` is the size of the range to be unmapped, in bytes.
///
/// # Safety
/// This function is unsafe because unmapping memory that is still in use (e.g., memory that is currently mapped and being accessed) can lead to undefined behavior.
/// The caller must ensure that unmapped memory is completely unused and will not be accessed after being unmapped to avoid issues such as use-after-free or memory corruption.
pub unsafe fn unmap(range: MemoryRange<VirtAddr>) -> Result<(), MemError> {
    unsafe { paging::unmap_unchecked(range) }
}

/// Allocates a virtual address range of the specified size without mapping it to any physical memory.
#[must_use = "The returned virtual address must be freed with `free_virtspace` when it is no longer needed to avoid memory leaks and ensure proper resource management."]
pub fn reserve_virtual(layout: Layout) -> Result<MemoryRange<VirtAddr>, MemError> {
    if layout.size() > arch::VIRTUAL_ADDRESS_MAX as usize {
        return Err(MemError::OutOfMemory);
    }

    let mut vmm = asm::vmm()?;
    vmm.allocate(layout).ok_or(MemError::OutOfMemory)
}

/// Frees a virtual address range of the specified size that was previously allocated with `alloc_virtspace`.
///
///
/// # Safety
/// The caller must ensure that the provided virtual address range is not currently mapped to any physical memory and is not in use before freeing it,
/// as freeing a virtual address range that is still in use can lead to undefined behavior such as use-after-free or memory corruption.
pub unsafe fn free_virtual(
    virt_range: MemoryRange<VirtAddr>,
    layout: Layout,
) -> Result<(), MemError> {
    let mut vmm = asm::vmm()?;
    // SAFETY: Guaranteed by caller.
    unsafe { vmm.deallocate(virt_range, layout) };

    Ok(())
}

/// Reserves a physical frame of the specified size and returns it to the caller.
pub fn reserve_frame<S: FragmentSize>() -> Result<Frame<S>, MemError> {
    let mut pmm = asm::pmm();

    pmm.allocate_fragment()
}

/// Frees a physical frame that was previously reserved with `reserve_frame`.
pub fn free_frame<S: FragmentSize>(frame: Frame<S>) {
    let mut pmm = asm::pmm();

    pmm.deallocate_fragment(frame);
}

/// A structure representing a mapping between a virtual address range and a physical address range, along with the size of the mapping in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvidedMemoryMapping {
    mapping: MemoryMapping,
    anon_virt_backed: bool,
}

impl ProvidedMemoryMapping {
    /// Creates a new `MemoryMapping` instance with the specified virtual base address, physical base address, and size in bytes.
    pub(crate) fn new(
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        size: u64,
        anon_virt_backed: bool,
    ) -> Option<Self> {
        MemoryMapping::new(virt_start, phys_start, size).map(|mapping| Self {
            mapping,
            anon_virt_backed,
        })
    }

    /// Creates a new `ProvidedMemoryMapping` instance from an existing `MemoryMapping` and a flag indicating whether the mapping is backed by anonymous virtual memory.
    pub(crate) fn from_mapping(mapping: MemoryMapping, anon_virt_backed: bool) -> Self {
        Self {
            mapping,
            anon_virt_backed,
        }
    }

    pub(crate) fn defer_deallocate_vm(mut self, should_deallocate_vm: bool) -> Self {
        self.anon_virt_backed = should_deallocate_vm;
        self
    }

    /// Returns an immutable pointer to the start of the mapped virtual address range, allowing for direct access to the mapped memory.
    pub fn as_ptr<T>(&self) -> *const T {
        self.mapping.virt_range().start().as_ptr()
    }

    /// Returns a mutable pointer to the start of the mapped virtual address range, allowing for direct access to the mapped memory.
    pub fn as_mut_ptr<T>(&self) -> *mut T {
        self.mapping.virt_range().start().as_mut_ptr()
    }
}

fn make_layout_for_mapping(phys_range: MemoryRange<PhysAddr>) -> Layout {
    let phys_base = phys_range.start();
    let size = phys_range.size() as usize;
    let alignment = phys_base.alignment();
    Layout::from_size_align(size, alignment.as_usize()).unwrap()
}

/// Maps a physical address range to a virtual address range of the specified size with the given flags, where the virtual address is allocated by the memory manager. This is a convenience function that combines `alloc_paged` and `map` into a single operation for ease of use.
#[must_use = "The returned virtual address must be freed with `unmap` when it is no longer needed to avoid memory leaks and ensure proper resource management."]
pub fn create_phys_mapping(
    phys_range: MemoryRange<PhysAddr>,
    flags: MapFlags,
) -> Result<ProvidedMemoryMapping, ContextualizedError> {
    let virt_range = reserve_virtual(make_layout_for_mapping(phys_range))?;
    map_direct(
        MemoryMapping::new(virt_range.start(), phys_range.start(), phys_range.size()).unwrap(),
        flags,
        None,
        &mut (),
    )
    .map(|m| m.defer_deallocate_vm(true))
    .map_err(|e| e.into())
}

/// Frees a physical memory mapping that was previously created with `create_phys_mapping`, unmapping the virtual address range and freeing the allocated virtual address space.
pub unsafe fn free_phys_mapping(mapping: ProvidedMemoryMapping) -> Result<(), ContextualizedError> {
    unsafe {
        unmap(mapping.mapping.virt_range())?;
        if mapping.anon_virt_backed {
            free_virtual(
                mapping.mapping.virt_range(),
                make_layout_for_mapping(mapping.mapping.phys_range()),
            )?;
        }
    }

    Ok(())
}

bitflags! {
    /// Flags for memory mapping, such as read/write permissions and caching behavior.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MapFlags: u64 {
        // PRESENT and it's forms are implicit by the usage of any map/alloc function
        // GLOBAL is similar, any allocation or mapping is implicitly global if it's in kernel space.
        /// Page is writable (if not set, the page is read-only)
        const WRITABLE = 1 << 1;
        /// Page can be accessed from user mode (if not set, the page is only accessible from kernel mode)
        const USER_ACCESSIBLE = 1 << 2;
        /// Marks the page as executable - this is only relevant on platforms with support
        /// for executable page permissions (e.g., x86_64 with the NX bit)
        const EXECUTABLE = 1 << 3;
        /// Disable caching for this page
        const CACHE_DISABLE = 1 << 4;
        /// Deallocate the physical memory backing this mapping when unmapping it
        const DEALLOCATE = 1 << 5;
        /// Marks the page as global, meaning it is not flushed from the TLB on a context switch
        const GLOBAL = 1 << 6;
    }
}

impl Display for MapFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.contains(MapFlags::WRITABLE) {
            write!(f, "W")?;
        } else {
            write!(f, "R")?;
        };

        if self.contains(MapFlags::USER_ACCESSIBLE) {
            write!(f, " US")?;
        } else {
            write!(f, " KS")?;
        };

        if self.contains(MapFlags::EXECUTABLE) {
            write!(f, " X")?;
        } else {
            write!(f, " NX")?;
        };

        if self.contains(MapFlags::CACHE_DISABLE) {
            write!(f, " NC")?;
        } else {
            write!(f, " C")?;
        };

        Ok(())
    }
}

impl Default for MapFlags {
    fn default() -> Self {
        Self::empty()
    }
}
/// Aligns the given value up or down to the nearest multiple of the specified alignment. The alignment must be a power of two.
///
/// # Usage
///
/// ```
/// # use nmm::align;
/// let aligned_up = align!(up, 0x1234, 0x1000); // Aligns 0x1234 up to the nearest multiple of 0x100
/// let aligned_down = align!(down, 0x1234, 0x1000); // Aligns 0x1234 down to the nearest multiple of 0x100
///
/// assert_eq!(aligned_up, 0x2000);
/// assert_eq!(aligned_down, 0x1000);
///   ```
#[macro_export]
macro_rules! align {
    // TODO: $value: lit cases so the alignment check isn't at runtime for constant values?
    (up, $value: expr, $alignment: expr) => {
        // bit fiddling method requires pow2
        {
            if !($alignment as u64).is_power_of_two() {
                panic!("Alignment must be a power of two");
            }
            ($value + ($alignment - 1)) & !($alignment - 1)
        }
    };

    (down, $value: expr, $alignment: expr) => {{
        if !($alignment as u64).is_power_of_two() {
            panic!("Alignment must be a power of two");
        }
        $value & !($alignment - 1)
    }};
}

cake::encapsulate_macro!(
    pub(crate) test_println,
    _test_println_mod,
    /// Expands to a print statement that is only included in test builds, allowing for debug printing in tests without affecting release builds.
    macro_rules! test_println {
        ($($arg:tt)*) => {
            #[cfg(test)]
            println!($($arg)*);
        };
    }
);

///
/// Recursively matches against a `MemError` and its variants, allowing for pattern matching on nested error types (without specifying it twice)
///
/// # Example
///
/// ```
/// let err = mem_err!(MalformedPageStructure::PagetableNotPresent);
/// match err {
///     mem_err!(MalformedPageStructure::PagetableNotPresent) => {
///         println!("It works!");
///     }
/// }
/// ```
///
#[macro_export]
macro_rules! mem_err {
    ($variant:ident :: $($rest:tt)+) => {
        $crate::MemError::$variant($crate::mem_err!(@inner $variant :: $($rest)+))
    };

    ($variant:ident $(($($pattern:tt)*))?) => {
        $crate::MemError::$variant$(($($pattern)*))?
    };

    (@inner $variant:ident :: $sub_variant:ident ($($pattern:tt)*)) => {
        $crate::err::$variant::$sub_variant($($pattern)*)
    };

    (@inner $variant:ident :: $sub_variant:ident ) => {
        $crate::err::$variant::$sub_variant
    };

    (Err $variant:ident $(($($pattern:tt)*))?) => {
        Err($crate::MemError::$variant$($($pattern)*)?)
    };

    (Err $variant:ident :: $($rest:tt)+) => {
        Err($crate::MemError::$variant($crate::mem_err!(@inner $variant :: $($rest)+)))
    };

}
