use cake::log::{debug, info};

use crate::{
    ContextualizedError, InitConfig, MapFlags, MemError, align,
    arch::{self, L1_PAGE_SIZE, pml4_phys},
    bitmap::{BitmapBacking, PhysicalMemoryManager, VirtualMemoryManager},
    entry_walker::EntryWalker,
    err::Contextualize,
    paging::{
        Address, AddressExt, Frame, PageTable, Small, accessor,
        asm::{self, InactiveAddressSpace},
        map::DataAllocator,
        map_from,
    },
};

pub(crate) unsafe fn init_unchecked(
    mut walker: EntryWalker<'static>,
    config: InitConfig,
) -> Result<(), ContextualizedError> {
    if config.managed_range.size() < arch::L1_PAGE_SIZE * 16 {
        return Err(MemError::ScratchSpaceTooSmall {
            provided: config.managed_range.size() as u64,
            required: arch::L1_PAGE_SIZE as u64 * 16,
        })
        .with_ctx("managed range invalid");
    }

    let cr3: Frame<Small> = pml4_phys();
    let root: &'static mut PageTable = unsafe {
        &mut *(accessor::build_vaddress(
            config.recursive_idx,
            config.recursive_idx,
            config.recursive_idx,
            config.recursive_idx,
        )
        .as_mut_ptr::<PageTable>())
    };

    for (i, entry) in root.entries().chunks_exact(4).enumerate() {
        debug!(
            "pml4[{}..{}]: {} {} {} {}",
            i * 4,
            (i * 4) + 4,
            entry[0],
            entry[1],
            entry[2],
            entry[3]
        );
    }

    // Initialize the mapper and set it as the active mapper for the system.
    // This is necessary to perform any virtual memory operations, including mapping the scratch space.
    let bootstrap_table =
        unsafe { InactiveAddressSpace::bootstrap(cr3, config.zero_page, config.recursive_idx) };
    unsafe {
        bootstrap_table
            .activate()
            .with_ctx("failed to activate bootstrap table")?
    };

    info!("Found {} bytes of usable memory", walker.usable_memory());

    // Always make sure the number of bytes will be aligned to u64
    let n_pages = (config.managed_range.size() as u64).div_ceil(L1_PAGE_SIZE);
    let n_bytes = align!(up, n_pages / 8, core::mem::size_of::<u64>() as u64);
    let n_entries = n_bytes / core::mem::size_of::<u64>() as u64;

    info!(
        "Mapping scratch space: base={:#x}, size={} bytes, pages={}, entries={}",
        config.managed_range.start().as_u64(),
        n_bytes,
        n_pages,
        n_entries
    );
    unsafe {
        map_from(
            config.managed_range.truncate(n_bytes),
            MapFlags::WRITABLE,
            None,
            &mut DataAllocator(&mut walker),
            &mut (),
        )
        .with_ctx("failed to map managed range")?;
    }

    let entries = unsafe {
        core::slice::from_raw_parts_mut(
            config.managed_range.start().as_mut_ptr::<u64>(),
            n_entries as usize,
        )
    };

    info!("Initializing virtual memory manager with scratch space");
    let mut vmm = unsafe {
        VirtualMemoryManager::init(
            BitmapBacking::ManuallyManaged(entries),
            config.managed_range,
        )
    };
    unsafe { vmm.mark_allocated(config.managed_range.start(), n_bytes) }

    info!("Initializing physical memory manager with scratch space");
    let pmm = unsafe {
        PhysicalMemoryManager::init(walker, &mut vmm)
            .with_ctx("failed to init physical memory manager")?
    };
    info!("Physical memory manager initialized successfully");

    asm::set_vmm(vmm);
    asm::set_pmm(pmm);

    info!("Memory manager initialized successfully");
    //Err(MemError::Uninit("todo"))
    Ok(())
}
