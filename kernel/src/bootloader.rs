//! Various bootloader requests and their corresponding responses. Contains most of the bootloader interface.
use core::convert::Infallible;

use alloc::vec::Vec;
use arrayvec::ArrayVec;
use cake::Once;
use cake::limine::BaseRevision;
use cake::limine::memory_map::Entry;
use cake::limine::mp::{Cpu, GotoAddress};
use cake::limine::{paging::Mode, request::*, response::ExecutableAddressResponse};
use cake::log::info;
use tmm::MapFlags;
use tmm::paging::heapless::HeaplessAllocator;
use tmm::paging::primitives::PhysRange;
use tmm::paging::{Address, PhysAddr, VirtAddr};

use crate::STACK_SIZE;
use crate::declare_module;

#[used]
static PHYSICAL_MEMORY_OFFSET_REQUEST: HhdmRequest = HhdmRequest::new();
#[used]
static RSDP_ADDRESS_REQUEST: RsdpRequest = RsdpRequest::new();
#[used]
static PAGING_MODE_REQUEST: PagingModeRequest =
    PagingModeRequest::new().with_mode(Mode::FOUR_LEVEL);
#[used]
static EXECUTABLE_ADDRESS_REQUEST: ExecutableAddressRequest = ExecutableAddressRequest::new();
#[used]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(3);
#[used]
static DATE_AT_BOOT: DateAtBootRequest = DateAtBootRequest::new();
/// Memory map provided by the bootloader
#[used]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

/// Kernel ELF provided by the bootloader
#[used]
static KERNEL_ELF: ExecutableFileRequest = ExecutableFileRequest::new();

/// Framebuffer provided by the bootloader
static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();

/// MP info provided by the bootloader
static MP_INFO: MpRequest = MpRequest::new();

static BOOT_TIMESTAMP: Once<u64> = Once::new();

/// Physical memory offset provided by the bootloader
static PHYSICAL_MEMORY_OFFSET: Once<VirtAddr> = Once::new();

/// Root System Description Pointer provided by the bootloader
static RSDP_ADDRESS: Once<Option<PhysAddr>> = Once::new();

/// Executable address provided by the bootloader
static EXECUTABLE_ADDRESS: Once<KernelAddress> = Once::new();

static MEMORY_MAP: Once<ArrayVec<Entry, 64>> = Once::new();

static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest::new().with_size(STACK_SIZE);

/// Returns the boot timestamp provided by the bootloader.
pub fn boot_timestamp() -> u64 {
    *BOOT_TIMESTAMP
        .get()
        .expect("BOOT_TIMESTAMP not initialized")
}

/// Returns the physical memory offset provided by the bootloader.
pub fn physical_memory_offset() -> VirtAddr {
    *PHYSICAL_MEMORY_OFFSET
        .get()
        .expect("PHYSICAL_MEMORY_OFFSET not initialized")
}

/// Returns the physical address of the RSDP provided by the bootloader, if any.
pub fn rsdp_address() -> Option<PhysAddr> {
    *RSDP_ADDRESS.get().expect("RSDP_ADDRESS not initialized")
}

/// The kernel's executable address in memory, provided by the bootloader.
#[derive(Debug, Clone, Copy)]
pub struct KernelAddress {
    /// The virtual address of the kernel's executable in memory.
    pub mut(self) virt_base: VirtAddr,
    /// The physical address of the kernel's executable in memory.
    pub mut(self) phys_base: PhysAddr,
}

/// Returns the kernel's executable address provided by the bootloader.
pub fn executable_address() -> KernelAddress {
    *EXECUTABLE_ADDRESS
        .get()
        .expect("EXECUTABLE_ADDRESS not initialized")
}

pub fn memory_map() -> &'static [Entry] {
    MEMORY_MAP
        .get()
        .expect("MEMORY_MAP not initialized")
        .as_slice()
}

/// Returns true if the bootloader fulfilled the stack size request, false otherwise.
pub fn did_fulfill_stack_size_request() -> bool {
    STACK_SIZE_REQUEST.get_response().is_some()
}

fn init() -> Result<(), Infallible> {
    if STACK_SIZE_REQUEST.get_response().is_none() {
        panic!("Bootloader did not provide stack size request response");
    }
    let offset = PHYSICAL_MEMORY_OFFSET_REQUEST
        .get_response()
        .unwrap()
        .offset();
    PHYSICAL_MEMORY_OFFSET.call_once(|| VirtAddr::new_truncate(offset));

    MEMORY_MAP.call_once(|| {
        let mmap = MEMORY_MAP_REQUEST.get_response().unwrap();

        let mut entries = ArrayVec::<Entry, 64>::new();
        for entry in mmap.entries() {
            entries.push(**entry);
        }

        entries
    });

    let exec_addr = EXECUTABLE_ADDRESS_REQUEST.get_response().unwrap();
    EXECUTABLE_ADDRESS.call_once(|| KernelAddress {
        virt_base: VirtAddr::new(exec_addr.virtual_base()),
        phys_base: PhysAddr::new(exec_addr.physical_base()),
    });

    RSDP_ADDRESS.call_once(|| {
        RSDP_ADDRESS_REQUEST
            .get_response()
            .map(|r| PhysAddr::new(r.address() as u64))
    });

    cake::set_kernel_elf(unsafe { read_limine_elf() }).unwrap();

    BOOT_TIMESTAMP.call_once(|| DATE_AT_BOOT.get_response().unwrap().timestamp().as_secs());
    cake::add_entropy_source(|| Some(BOOT_TIMESTAMP.get().unwrap().clone()));
    Ok(())
}

static KERNEL_ELF_OWNED: Once<Vec<u8, HeaplessAllocator>> = Once::new();
static MULTIPROC_INFO: Once<Vec<CoreData, HeaplessAllocator>> = Once::new();

/// Information about a core provided by the bootloader.
#[derive(Debug, Clone, Copy)]
pub struct CoreData {
    /// The core's APIC ID as assigned by the bootloader.
    pub lapic_id: u32,
    /// The core's ID as assigned by the bootloader.
    pub core_id: u32,
}

/// Returns the kernel ELF provided by bootloader. The caller must ensure that this is still mapped in the current address space.
pub unsafe fn read_limine_elf() -> &'static [u8] {
    let file = KERNEL_ELF.get_response().unwrap().file();
    unsafe { core::slice::from_raw_parts(file.addr(), file.size() as usize) }
}
/// Returns the multiprocessor information provided by the bootloader. The caller must ensure that this is still mapped in the current address space.
pub unsafe fn read_limine_mp_info() -> &'static [&'static Cpu] {
    let mp_info = MP_INFO.get_response().unwrap();
    mp_info.cpus()
}

/// Copies the kernel ELF and multiprocessor information provided by the bootloader into owned memory so that it can be accessed after the kernel is remapped.
pub fn copy_to_owned_memory() {
    let kernel_elf = unsafe { read_limine_elf() };
    let mut vec = Vec::<u8, HeaplessAllocator>::new_in(HeaplessAllocator::new(MapFlags::WRITABLE));
    vec.extend_from_slice(kernel_elf);
    KERNEL_ELF_OWNED.call_once(|| vec);

    let _ = cake::set_kernel_elf(&*KERNEL_ELF_OWNED.get().unwrap());

    let mp_info = MP_INFO.get_response().unwrap();
    let mut cores =
        Vec::<CoreData, HeaplessAllocator>::new_in(HeaplessAllocator::new(MapFlags::WRITABLE));
    for cpu in mp_info.cpus() {
        cores.push(CoreData {
            lapic_id: cpu.lapic_id,
            core_id: cpu.id,
        });
    }
    MULTIPROC_INFO.call_once(|| cores);
}

declare_module!("requests", init);
