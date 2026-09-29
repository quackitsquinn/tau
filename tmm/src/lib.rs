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

use bitflags::bitflags;
use cake::limine::memory_map;

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
mod init;
pub mod kernel_map;
pub mod paging;

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
pub unsafe fn init(init_config: InitConfig) -> Result<(), MemError> {
    unsafe { init::init_unchecked(EntryWalker::new(init_config.memory_map)?, init_config) }
}

/// A source of physical memory for mapping virtual addresses.
/// This enum defines the various ways in which physical memory can be allocated or copied when creating a new mapping in the virtual address space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum MapSource {
    /// Maps the given physical address directly to the virtual address.
    Direct(PhysAddr),
    /// Allocate physical memory for the mapping.
    Anon,
}

/// Maps a virtual address range to a physical address range with the specified size
///
/// - `dest` is the starting virtual address of the range to be mapped.
/// - `phys_base` is the starting physical address of the range to be mapped.
/// - `byte_size` is the size of the range to be mapped, in bytes.
/// - `flags` are the mapping flags that specify the permissions and attributes of the mapping (e.g., read/write permissions, caching behavior).
// I thought about making this unsafe since it can cause undefined behavior BUT
// said undefined behavior requires dereferencing the mapped memory, so the safety is the caller's responsibility.
pub fn map(
    dest: MemoryRange<VirtAddr>,
    src: MapSource,
    flags: MapFlags,
    parent_table_flags: Option<MapFlags>,
    op: &mut impl OperationAllSizes,
) -> Result<(), MemError> {
    if let MapSource::Direct(phys_base) = src {
        check_range_phys(phys_base, dest.size() as usize)?;
    }

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
pub struct MemoryMapping {
    /// The starting virtual address of the mapped range.
    pub mut(crate) virt_range: VirtRange,
    /// The range of physical memory that is mapped.
    pub mut(crate) phys_range: PhysRange,
}

impl MemoryMapping {
    /// Creates a new `MemoryMapping` instance with the specified virtual base address, physical base address, and size in bytes.
    pub fn new(virt_range: VirtRange, phys_range: MemoryRange<PhysAddr>) -> Self {
        Self {
            virt_range,
            phys_range,
        }
    }

    /// Returns an immutable pointer to the start of the mapped virtual address range, allowing for direct access to the mapped memory.
    pub fn as_ptr<T>(&self) -> *const T {
        self.virt_range.start().as_ptr()
    }

    /// Returns a mutable pointer to the start of the mapped virtual address range, allowing for direct access to the mapped memory.
    pub fn as_mut_ptr<T>(&self) -> *mut T {
        self.virt_range.start().as_mut_ptr()
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
) -> Result<MemoryMapping, MemError> {
    let virt_range = reserve_virtual(make_layout_for_mapping(phys_range))?;
    unsafe {
        paging::map_unchecked(
            virt_range,
            MapSource::Direct(phys_range.start()),
            flags,
            None,
            &mut (),
        )
    }?;
    Ok(MemoryMapping::new(virt_range, phys_range))
}

/// Frees a physical memory mapping that was previously created with `create_phys_mapping`, unmapping the virtual address range and freeing the allocated virtual address space.
pub unsafe fn free_phys_mapping(mapping: MemoryMapping) -> Result<(), MemError> {
    unsafe {
        unmap(mapping.virt_range)?;
        free_virtual(
            mapping.virt_range,
            make_layout_for_mapping(mapping.phys_range),
        )
    }
}

/// The reason why a virtual address range was rejected as invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidRangeReason {
    /// The address range would overflow when calculating the end address (i.e., `virt_base + byte_size` would overflow).
    Overflow,
    /// The address range is not canonical for the architecture (e.g., on x86_64, the upper bits beyond the architecture's address width are not all 0s or all 1s).
    NonCanonical,
    /// The address range is not aligned to the required page size (i.e., `virt_base` is not a multiple of the page size, or `byte_size` is not a multiple of the page size).
    Unaligned,
}

/// An error that can occur during memory mapping operations, such as invalid addresses, insufficient resources, or permission issues. The specific variants of this error type can be defined based on the needs of the memory manager implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MemError {
    /// The mapping operation failed because the provided range was already mapped to a different physical address. The provided physical address is included for reference.
    #[error(
        "The provided virtual address range is already mapped to a different physical address: {0:?}"
    )]
    AlreadyMapped(PhysAddr),
    /// The mapping operation failed because the system ran out of memory to complete the mapping.
    /// This could occur if there are no free physical pages available to back the mapping, or if the memory manager's internal data structures are exhausted.
    #[error("The mapping operation failed due to insufficient memory resources.")]
    OutOfMemory,
    /// The provided virtual address range is invalid, such as being out of bounds for the architecture or overflowing when calculating the end address.
    #[error("The provided virtual address range is invalid: {begin:?} with size {size} bytes")]
    InvalidVirtRange {
        /// The specific reason why the virtual address range is considered invalid (e.g., overflow, unaligned, etc.).
        reason: InvalidRangeReason,
        /// The starting virtual address of the invalid range.
        begin: VirtAddr,
        /// The size of the invalid range in bytes.
        size: u64,
    },
    /// The provided physical address range is invalid, such as being out of bounds for the architecture or overflowing when calculating the end address.
    #[error("The provided physical address range is invalid: {begin:?} with size {size} bytes")]
    InvalidPhysRange {
        /// The specific reason why the physical address range is considered invalid (e.g., overflow, unaligned, etc.).
        reason: InvalidRangeReason,
        /// The starting physical address of the invalid range.
        begin: PhysAddr,
        /// The size of the invalid range in bytes.
        size: u64,
    },
    /// The provided scratch space for initialization is too small to be used for the necessary virtual memory operations during initialization and `alloc_paged`.
    #[error(
        "The provided scratch space is too small: provided {provided} bytes, but at least {required} bytes are required."
    )]
    ScratchSpaceTooSmall {
        /// The size of the provided scratch space in bytes.
        provided: u64,
        /// The minimum required size of the scratch space in bytes.
        required: u64,
    },
    /// The required resources to complete the requested operation have not been initialized yet.
    #[error(
        "The required resources to complete the requested operation have not been initialized yet: {0}"
    )]
    Uninit(&'static str),
    /// The requested operation failed because the specified virtual address range is not currently mapped to any physical memory, and therefore cannot be unmapped or accessed.
    /// The provided virtual address is included for reference.
    #[error("The specified virtual address range is not currently mapped to any physical memory")]
    NotMapped(AnyFragment<PageClass>),
    /// The requested operation failed because a needed entry in the page table points to an invalid frame address.
    #[error("The page table entry points to an invalid frame address: {0:?}")]
    InvalidFrameAddress(PhysAddr),
    /// The provided alignment value is invalid (e.g., not a power of two).
    #[error("The provided alignment is invalid: {0} is not a power of two.")]
    InvalidAlignment(usize),
    /// The requested operation failed due to the provided virtual address not being managed by whatever resource is being accessed.
    #[error("The provided virtual address is not managed by the memory manager: {0:?}")]
    UnmanagedVirtual(
        /// The virtual address that is not managed by the memory manager.
        VirtAddr,
    ),
    /// The requested operation failed due to the provided physical address not being managed by whatever resource is being accessed.
    #[error("The provided physical address is not managed by the memory manager: {0:?}")]
    UnmanagedPhysical(
        /// The physical address that is not managed by the memory manager.
        PhysAddr,
    ),
    /// The requested operation failed because the provided virtual address is mapped to a higher-level page table entry,
    /// preventing the requested operation from completing.
    #[error(
        "The provided virtual address is mapped to a higher-level page table entry, preventing the requested operation from completing: {0:?}"
    )]
    MappedToHigherLevel(
        /// The virtual address that is mapped to a higher-level page table entry, preventing the requested operation from completing.
        VirtAddr,
    ),
    /// The requested operation failed because the provided virtual address is mapped to a lower-level page table entry,
    /// preventing the requested operation from completing.
    #[error(
        "The provided virtual address is mapped to a lower-level page table entry, preventing the requested operation from completing: {0:?}"
    )]
    MappedToLowerLevel(
        /// The virtual address that is mapped to a lower-level page table entry, preventing the requested operation from completing.
        VirtAddr,
    ),
    /// The requested operation failed because the required resources are unavailable, such as a missing page table or a second address space builder on the same address space.
    #[error("The requested operation failed because the required resources are unavailable: {0}")]
    ResourceUnavailable(&'static str),
    /// The requested operation failed because the required page table is not present in memory.
    #[error(
        "The requested operation failed because the required page table is not present in memory."
    )]
    PagetableNotPresent,
    /// The requested operation failed because a global page was encountered, which is not supported or disabled.
    #[error(
        "The requested operation failed because a global page was encountered, which is not supported or disabled."
    )]
    GlobalPageEncountered,
    /// An error that originated from architecture-specific operations in the memory manager.
    #[error("An architecture-specific error occurred during memory management operations: {0}")]
    ArchError(#[from] arch::ArchError),
    /// The requested operation is invalid in the current context, such as attempting to perform an operation that is not allowed or supported.
    #[error("The requested operation is invalid in the current context.")]
    InvalidOperation,
    /// The requested operation failed due to an error in the copy mappings operation, such as a failure to copy mappings between page tables or address spaces.
    #[error("The requested operation failed due to an error in the copy mappings operation: {0:?}")]
    CopyMappingError(#[from] CopyMappingError),
    /// An error that originated from the underlying memory mapping implementation, such as page table manipulation or low-level memory operations.
    #[error("An error occurred during memory management operations: {0}")]
    Other(&'static str),
}

impl MemError {
    pub(crate) fn scratch_space_too_small(provided: u64) -> Self {
        Self::ScratchSpaceTooSmall {
            provided,
            required: MIN_MANAGED_RANGE_SIZE,
        }
    }
}

const fn check_range_virt(virt_base: VirtAddr, byte_size: usize) -> Result<(), MemError> {
    // Check for overflow in the address range
    let val = virt_base.checked_add(byte_size as u64);
    match val {
        Some(_) => (),
        None => {
            return Err(MemError::InvalidVirtRange {
                reason: InvalidRangeReason::Overflow,
                begin: virt_base,
                size: byte_size as u64,
            });
        }
    };

    Ok(())
}

fn check_range_phys(phys_base: PhysAddr, byte_size: usize) -> Result<(), MemError> {
    // Check for overflow in the address range
    let val = phys_base.checked_add(byte_size as u64);
    match val {
        Some(_) => (),
        None => {
            return Err(MemError::InvalidPhysRange {
                reason: InvalidRangeReason::Overflow,
                begin: phys_base,
                size: byte_size as u64,
            });
        }
    };

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
