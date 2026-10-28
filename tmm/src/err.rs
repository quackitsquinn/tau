use cake::stacktrace::{self, StackTrace};

use crate::{
    MIN_MANAGED_RANGE_SIZE, arch,
    paging::{
        PhysAddr, VirtAddr,
        accessor::CopyMappingError,
        primitives::{AnyFragment, PageClass},
    },
};

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

/// An error that can occur during a memory mapping operation, when a strange or otherwise unexpected page structure is encountered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MalformedPageStructure {
    /// The mapping operation failed because the provided range was already mapped to a different physical address. The provided physical address is included for reference.
    #[error(
        "The provided virtual address range is already mapped to a different physical address: {0:?}"
    )]
    AlreadyMapped(PhysAddr),
    /// The requested operation failed because the specified virtual address range is not currently mapped to any physical memory, and therefore cannot be unmapped or accessed.
    /// The provided virtual address is included for reference.
    #[error("The specified virtual address range is not currently mapped to any physical memory")]
    NotMapped(AnyFragment<PageClass>),
    /// The requested operation failed because a needed entry in the page table points to an invalid frame address.
    #[error("The page table entry points to an invalid frame address: {0:?}")]
    InvalidFrameAddress(PhysAddr),
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
}

/// An error that can occur during memory mapping operations, such as invalid addresses, insufficient resources, or permission issues.
///
/// This should be the return type of simple leaf functions or functions that only have one possible error source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MemError {
    /// The mapping operation failed because the system ran out of memory to complete the mapping.
    /// This could occur if there are no free physical pages available to back the mapping, or if the memory manager's internal data structures are exhausted.
    #[error("The mapping operation failed due to insufficient memory resources.")]
    OutOfMemory,
    /// The mapping operation failed due to a malformed page structure, such as an invalid page table entry or an unexpected page table layout.
    #[error("The mapping operation failed due to a malformed page structure: {0:?}")]
    MalformedPageStructure(#[from] MalformedPageStructure),
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
    /// The requested operation failed because the required resources are unavailable, such as a missing page table or a second address space builder on the same address space.
    #[error("The requested operation failed because the required resources are unavailable: {0}")]
    ResourceUnavailable(&'static str),
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

#[derive(Debug, PartialEq, Eq)]
pub struct ContextualizedError {
    // Okay so I initially had two separate ideas for this:
    // 1. A "contextualized" error type is a type that calls the `contextualize` method on the error type to add context to the error.
    // 2. A "contextualized" error type is a type wraps a stack trace and a cause.
    // I liked the first idea more at first, since I have recently been working in Go and I like how implicit it is to chain errors in Go,
    // but the big point is easily finding the root cause of an error, and I think the second idea is better for that.
    // Both solutions provide slightly different benefits, but it's not enough to justify having both right now.
    cause: MemError,
    // TODO: If in the future there is a hotpath where we match err a lot, we can make this an Option<StackTrace> and only capture conditionally.
    trace: StackTrace<8>,
}

impl ContextualizedError {
    pub fn new(cause: MemError) -> Self {
        Self::from_raw_parts(cause, stacktrace::collect_stacktrace())
    }

    fn from_raw_parts(cause: MemError, trace: StackTrace<8>) -> Self {
        Self { cause, trace }
    }

    pub fn cause(&self) -> MemError {
        self.cause
    }

    pub fn trace(&self) -> &StackTrace<8> {
        &self.trace
    }
}

impl From<MemError> for ContextualizedError {
    fn from(err: MemError) -> Self {
        ContextualizedError::from_raw_parts(err, stacktrace::collect_stacktrace())
    }
}
