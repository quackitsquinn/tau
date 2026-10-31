use core::{
    alloc::Layout,
    arch::asm,
    convert::Infallible,
    mem::{self, Alignment},
    ptr::{self, addr_of},
};

use crate::{
    STACK_SIZE,
    bootloader::{self},
    declare_module,
};
use alloc::{alloc::Allocator, vec::Vec};
use cake::{
    log::{info, trace},
    trace_disabled,
};
use kalloc::{GlobalAllocator, HeapConfig};
use tmm::{
    ContextualizedError, InitConfig, MapFlags, MapSource, MemError,
    err::Contextualize,
    paging::{
        Address, AddressExt, MemoryFragment, MemoryRange, PageTable, PageTableEntry,
        PageTableIndex, VirtAddr, asm::InactiveAddressSpace, heapless::HeaplessAllocator,
    },
};

/// Enables or disables allocation debugging based on the ALLOC_DEBUG environment variable.
pub const ALLOC_DEBUG: bool = option_env!("ALLOC_DEBUG").is_some();

unsafe extern "C" {
    #[link_name = "kernel_start"]
    static KERNEL_START: u8;
    #[link_name = "kernel_end"]
    static KERNEL_END: u8;
}

/// Returns the memory range occupied by the kernel binary in virtual memory.
pub fn kernel_range() -> MemoryRange<VirtAddr> {
    let start = unsafe { Address::new(&KERNEL_START as *const u8 as u64) };
    let end = unsafe { Address::new(&KERNEL_END as *const u8 as u64) };
    MemoryRange::new(start, end)
}

pub fn stack_range() -> MemoryRange<VirtAddr> {
    let stack_top = *crate::STACK_TOP.get().expect("STACK_BASE not initialized");
    let stack_base = stack_top - crate::STACK_SIZE;
    MemoryRange::new(stack_base, stack_top)
}

declare_module!("memory", init);

const HEAP_CONFIG: HeapConfig = HeapConfig {
    initial_heap_size: 16 * 1024 * 1024, // 16 MiB
    max_heap_size: 256 * 1024 * 1024,    // 256 MiB
};

#[global_allocator]
static GLOBAL_ALLOCATOR: GlobalAllocator = GlobalAllocator::new(HEAP_CONFIG);

fn init() -> Result<(), Infallible> {
    let recursive_idx = match unsafe { init_tmm() } {
        Ok(idx) => idx,
        Err(e) => {
            panic!("Failed to initialize tmm: {}", e);
        }
    };

    match map_kernel(recursive_idx) {
        Err(e) => {
            panic!("Failed to map kernel: {:?}", e);
        }
    }
}

extern "C" fn exitpoint() -> ! {
    info!("Kernel memory mapped successfully");
    info!("Initializing kernel heap allocator...");

    match GLOBAL_ALLOCATOR.init() {
        Ok(_) => info!("Kernel heap allocator initialized successfully"),
        Err(e) => panic!("Failed to initialize kernel heap allocator: {:?}", e),
    }
    info!(
        "Kernel heap initialized with initial size: {} bytes",
        HEAP_CONFIG.initial_heap_size
    );
    crate::init_stage1()
}

fn map_kernel(recursive_idx: PageTableIndex) -> Result<!, MemError> {
    let kernel_range = kernel_range();

    let new_as = InactiveAddressSpace::new(recursive_idx)?;
    let mut builder = match new_as.try_mount() {
        Ok(mounted) => mounted,
        Err((e, _table)) => {
            panic!(
                "Failed to mount new address space for kernel mapping: {:?}",
                e
            );
        }
    };

    info!("reserving {:x} bytes for new stack", STACK_SIZE);
    let new_stack = tmm::reserve_virtual(
        Layout::from_size_alignment(
            STACK_SIZE as usize,
            Alignment::new(STACK_SIZE.next_power_of_two() as usize).unwrap(),
        )
        .unwrap(),
    )?;

    tmm::map(
        new_stack,
        MapSource::Anon,
        MapFlags::WRITABLE,
        None,
        &mut (),
    )?;

    info!(
        "Copying kernel mappings with kernel range: {:#x?}",
        kernel_range
    );
    builder.copy_mappings_from_base(kernel_range, None)?;
    builder.copy_mappings_from_base(map::tmm_managed_range::RANGE, None)?;

    let new_as = builder
        .unmount()
        .expect("Failed to unmount new address space");

    let l4_paddr = unsafe { tmm::paging::asm::prepare_inactive_space(new_as) }?;
    info!("Switching address spaces, hold your breath...");
    trace!(
        "Switching to new address space with PML4 at physical address: {:#x}",
        l4_paddr.start_address().as_u64()
    );
    unsafe {
        asm! {
            "mov cr3, r8",          // load cr3
            "mov rsp, rcx",         // update stack
            "xor rbp, rbp",         // clear rbp to avoid issues with stack unwinding
            "push 0",               // push a dummy return address to align the stack
            "jmp {exitpoint}",      // jump to exitpoint
            exitpoint = sym exitpoint,
            in("r8") l4_paddr.start_address().as_u64(), // cr3
            in("rcx") new_stack.end().as_u64(),         // new stack top
            options(noreturn)
        }
    }
}

unsafe fn init_tmm() -> Result<PageTableIndex, ContextualizedError> {
    let memory_map = bootloader::memory_map();
    let recursive_idx = unsafe { find_set_recursive_entry().unwrap() };

    let init = InitConfig::find_scratch_page(
        recursive_idx,
        map::tmm_managed_range::RANGE,
        bootloader::memory_map(),
    )
    .with_ctx("failed to create config")?;

    info!("Initializing tmm {:?}", init);
    unsafe { tmm::init(init) }?;
    info!("Memory manager initialized");

    // Copy the kernel mappings into the managed range so it isn't lost when the kernel is remapped.
    let alloc = HeaplessAllocator::new(MapFlags::WRITABLE);
    let kernel_elf =
        cake::kernel_elf().ok_or(MemError::Uninit("kernel elf").with_ctx("kernel elf"))?;
    let alloc = alloc.allocate(Layout::array::<u8>(kernel_elf.data.len()).unwrap())?;
    unsafe {
        ptr::copy_nonoverlapping(
            kernel_elf.data.as_ptr(),
            alloc.as_ptr().cast::<u8>(),
            kernel_elf.data.len(),
        );
    }
    let slice =
        unsafe { core::slice::from_raw_parts(alloc.as_ptr().cast::<u8>(), kernel_elf.data.len()) };
    drop(kernel_elf);
    cake::set_kernel_elf(slice).unwrap();

    Ok(recursive_idx)
}

const RECURSIVE_RANGE_START: PageTableIndex =
    PageTableIndex::new(PageTableIndex::MAX.0 - (PageTableIndex::MAX.0 / 4));
const RECURSIVE_RANGE_END: PageTableIndex =
    unsafe { PageTableIndex::new_unchecked(PageTableIndex::MAX.0 - 1) };

unsafe fn find_set_recursive_entry() -> Option<PageTableIndex> {
    let l4_phys = tmm::arch::pml4_phys();
    let pml4_vaddr = l4_phys
        .translate_offset(bootloader::physical_memory_offset())
        .expect("Failed to translate PML4 physical address to virtual address");
    let root = unsafe { &mut *(pml4_vaddr.as_mut_ptr::<PageTable>()) };
    for i in PageTableIndex::iter_range(RECURSIVE_RANGE_START..RECURSIVE_RANGE_END).rev() {
        if root.read_entry(i).is_present() {
            continue;
        }
        unsafe {
            root.set_entry(i, PageTableEntry::new(l4_phys, MapFlags::WRITABLE));
        }
        return Some(i);
    }
    None
}

/// Defines various memory map constants used by the kernel.
///
/// KERNEL_* = Kernel memory
///
/// KERNEL_HEAP_* = Kernel heap memory
///
/// KERNEL_PHYS_MAP_* = Kernel misc memory (e.g virtual/physical memory mapping)
///
/// KERNEL_BINARY = Kernel binary memory
///
/// HIGHER_HALF_START = Start of the higher half of the kernel memory
use tmm::kernel_map;

kernel_map! {
    . = (higher_half + 512 GiB),
    TMM_MANAGED_RANGE = 3 GiB; align 1 GiB,
    TMM_ZERO_PAGE = Large; align Large,
    KERNEL_HEAP = 16 MiB; align 2 MiB,
    KERNEL_PHYS_MAP = 256 MiB; align 2 MiB,
    KERNEL_REMAP = 256 MiB; align 2 MiB,
    FRAMEBUFFER = 2 MiB; align 2 MiB,
    ADDRESS_SPACE_INFO = 4 KiB; align 4 KiB,
}
