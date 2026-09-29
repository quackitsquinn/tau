use core::{
    alloc::Layout,
    arch::asm,
    convert::Infallible,
    mem,
    ptr::{self, addr_of},
};

use crate::{
    declare_module,
    requests::{MEMORY_MAP, PHYSICAL_MEMORY_OFFSET},
};
use alloc::vec::Vec;
use cake::{
    log::{info, trace},
    trace_disabled,
};
use kalloc::{GlobalAllocator, HeapConfig};
use tmm::{
    InitConfig, MapFlags, MemError,
    paging::{
        Address, AddressExt, MemoryFragment, MemoryRange, PageTable, PageTableEntry,
        PageTableIndex, VirtAddr, asm::InactiveAddressSpace, heapless::HeaplessAllocator,
    },
};

pub mod elf_req_data;
pub mod req_data;

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
    let recursive_idx = unsafe { init_tmm().unwrap() };
    map_kernel(recursive_idx).unwrap();
    info!("Kernel memory mapped successfully");
    info!("Initializing kernel heap allocator...");
    assert!(GLOBAL_ALLOCATOR.init().unwrap());
    info!(
        "Kernel heap initialized with initial size: {} bytes",
        HEAP_CONFIG.initial_heap_size
    );
    crate::hlt_loop();
    Ok(())
}

fn map_kernel(recursive_idx: PageTableIndex) -> Result<(), MemError> {
    let kernel_range = kernel_range();
    let stack_range = stack_range();

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

    info!("reserving {:x} bytes for new stack", stack_range.size());
    let new_stack = tmm::reserve_virtual(
        Layout::from_size_alignment(stack_range.size() as usize, stack_range.start().alignment())
            .unwrap(),
    )?;

    assert!(new_stack.size() == stack_range.size());

    info!(
        "Copying kernel mappings with kernel range: {:#x?}",
        kernel_range
    );
    builder.copy_mappings_from_base(kernel_range, None)?;
    info!(
        "Copying stack mappings from old stack range: {:#x?} to new stack range: {:#x?}",
        stack_range, new_stack
    );
    builder.copy_mappings_from_base(stack_range, Some(new_stack))?;
    builder.copy_mappings_from_base(map::tmm_managed_range::RANGE, None)?;

    // Something I didn't initially think of: we have to update the frame pointers.
    // If a panic happens, it will instantly deref unmapped memory and crash.

    let mut current_frame = cake::stacktrace::root_frame();
    let mut frame = unsafe { cake::stacktrace::read_frame(current_frame) };
    while let Some(f) = frame {
        let old_frame_addr =
            VirtAddr::from_mut_ptr(f.last_frame).expect("failed to convert frame addr");
        let stack_offset = stack_range.end().as_u64() - old_frame_addr.as_u64();
        let new_frame_addr = new_stack.end() - stack_offset;
        let new_frame = cake::stacktrace::StackFrame {
            last_frame: if old_frame_addr.is_null() {
                ptr::null_mut()
            } else {
                new_frame_addr.as_mut_ptr()
            },
            instruction_pointer: f.instruction_pointer,
        };
        trace_disabled!(
            "kernel.mem.frameremap",
            "Remapping frame pointer from {:#x} to {:#x}",
            old_frame_addr.as_u64(),
            new_frame.last_frame as u64
        );
        unsafe {
            cake::stacktrace::write_frame(current_frame, new_frame);
        }

        current_frame = f.last_frame;
        frame = unsafe { cake::stacktrace::read_frame(current_frame) };
    }

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
            "mov r10, rax",        // r10 = old_stack_top
            "sub r10, rsp",       // r10 = old_stack_top - rsp (the offset of the current stack pointer from the top of the old stack)
            "mov r9, rcx",        // r9 = new_stack_top
            "sub r9, r10",        // r9 = new_stack_top - old_stack_offset (the new stack pointer position)

            "mov r10, rax",       // r10 = old_stack_top
            "sub r10, rbp",       // r10 = old_stack_top - rbp (the offset of the current frame pointer from the top of the old stack)
            "mov r11, rcx",       // r11 = new_stack_top
            "sub r11, r10",       // r11 = new_stack_top - old_stack_offset (the new frame pointer position)

            "mov cr3, r8",        // load cr3
            "mov rsp, r9",        // update stack
            "mov rbp, r11",       // update frame pointer
            in("r8") l4_paddr.start_address().as_u64(), // cr3
            in("rcx") new_stack.end().as_u64(),         // new stack top
            in("rax") stack_range.end().as_u64(),       // old stack top
        }
    }
    info!("Address space switched successfully!");

    Ok(())
}

unsafe fn init_tmm() -> Result<PageTableIndex, MemError> {
    let memory_map = MEMORY_MAP.lock_limine();
    let memory_map = memory_map.entries();
    let recursive_idx = unsafe { find_set_recursive_entry().unwrap() };

    let init =
        InitConfig::find_scratch_page(recursive_idx, map::tmm_managed_range::RANGE, unsafe {
            mem::transmute(memory_map)
        })?;

    info!("Initializing tmm {:?}", init);
    unsafe { tmm::init(init) }?;
    info!("Memory manager initialized");
    Ok(recursive_idx)
}

const RECURSIVE_RANGE_START: PageTableIndex =
    PageTableIndex::new(PageTableIndex::MAX.0 - (PageTableIndex::MAX.0 / 4));
const RECURSIVE_RANGE_END: PageTableIndex =
    unsafe { PageTableIndex::new_unchecked(PageTableIndex::MAX.0 - 1) };

unsafe fn find_set_recursive_entry() -> Option<PageTableIndex> {
    let l4_phys = tmm::arch::pml4_phys();
    let pml4_vaddr = l4_phys
        .translate_offset(*PHYSICAL_MEMORY_OFFSET.get().unwrap())
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
    TMM_MANAGED_RANGE = 2 GiB; align 1 GiB,
    TMM_ZERO_PAGE = Large; align Large,
    KERNEL_HEAP = 16 MiB; align 2 MiB,
    KERNEL_PHYS_MAP = 256 MiB; align 2 MiB,
    KERNEL_REMAP = 256 MiB; align 2 MiB,
    FRAMEBUFFER = 2 MiB; align 2 MiB,
    ADDRESS_SPACE_INFO = 4 KiB; align 4 KiB,
}
