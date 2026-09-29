//! Various bootloader requests and their corresponding responses. Contains most of the bootloader interface.
use core::convert::Infallible;

use cake::limine::BaseRevision;
use cake::limine::{paging::Mode, request::*, response::ExecutableAddressResponse};
use cake::log::info;
use cake::{LimineRequest, Once};
use tmm::paging::{Address, VirtAddr};

use crate::STACK_SIZE;
use crate::{
    declare_module,
    display::req_data::FramebufferInfo,
    memory::{elf_req_data::KernelElf, req_data::MemoryMap},
    mp::ApplicationCores,
};

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
static BOOT_TIMESTAMP: Once<u64> = Once::new();

/// Physical memory offset provided by the bootloader
pub static PHYSICAL_MEMORY_OFFSET: Once<VirtAddr> = Once::new();

/// Root System Description Pointer provided by the bootloader
pub static RSDP_ADDRESS: Once<Option<usize>> = Once::new();

/// Memory map provided by the bootloader
#[used]
pub static MEMORY_MAP: LimineRequest<MemoryMapRequest, MemoryMap> =
    LimineRequest::new(MemoryMapRequest::new());

/// Kernel ELF provided by the bootloader
#[used]
pub static KERNEL_ELF: LimineRequest<ExecutableFileRequest, KernelElf> =
    LimineRequest::new(ExecutableFileRequest::new());

/// Framebuffer provided by the bootloader
pub static FRAMEBUFFER: LimineRequest<FramebufferRequest, FramebufferInfo> =
    LimineRequest::new(FramebufferRequest::new());

/// MP info provided by the bootloader
pub static MP_INFO: LimineRequest<MpRequest, ApplicationCores> =
    LimineRequest::new(MpRequest::new());

/// Executable address provided by the bootloader
pub static EXECUTABLE_ADDRESS: Once<&'static ExecutableAddressResponse> = Once::new();

static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest::new().with_size(STACK_SIZE);

fn init() -> Result<(), Infallible> {
    if STACK_SIZE_REQUEST.get_response().is_none() {
        panic!("Bootloader did not provide stack size request response");
    }
    let offset = PHYSICAL_MEMORY_OFFSET_REQUEST
        .get_response()
        .unwrap()
        .offset();
    PHYSICAL_MEMORY_OFFSET.call_once(|| VirtAddr::new_truncate(offset));

    MEMORY_MAP.init(MemoryMap::new);

    KERNEL_ELF.init(KernelElf::new);

    FRAMEBUFFER.init(FramebufferInfo::new);

    MP_INFO.init(|i| {
        let n_cores = i.cpus().len();
        info!("Bootloader provided {} cores", n_cores);
        unsafe { cake::set_core_count(n_cores) };
        ApplicationCores::new(i)
    });

    let exec_addr = EXECUTABLE_ADDRESS_REQUEST.get_response().unwrap();
    EXECUTABLE_ADDRESS.call_once(|| exec_addr);

    RSDP_ADDRESS.call_once(|| RSDP_ADDRESS_REQUEST.get_response().map(|r| r.address()));

    let lock = KERNEL_ELF.lock_limine();
    let file = lock.file();
    cake::set_kernel_elf(unsafe {
        core::slice::from_raw_parts(file.addr() as *const u8, file.size() as usize)
    })
    .unwrap()
    .unwrap();

    BOOT_TIMESTAMP.call_once(|| DATE_AT_BOOT.get_response().unwrap().timestamp().as_secs());
    cake::add_entropy_source(|| Some(BOOT_TIMESTAMP.get().unwrap().clone()));
    Ok(())
}

declare_module!("requests", init);
