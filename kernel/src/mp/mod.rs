//! Multiprocessor setup and processor local APIC management.
use core::convert::Infallible;

use bitfield::{BitMut, BitRangeMut};
use cake::log::info;
use raw_cpuid::CpuId;

use crate::{
    declare_module,
    gdt::LGDT,
    interrupts::{self, KernelInterrupt, hardware},
    mp::{
        ioapic::IoApic,
        lapic::{LAPIC_BASE_MSR, Lapic},
        mp_setup::{dispatch_all, dispatch_others, trampoline::core_wait},
    },
};

pub mod ioapic;
pub mod lapic;

mod core_local;
mod mp_setup;

pub use mp_setup::{
    CoreContext, MODULE as PREINIT_MODULE, cores, dispatch_to, is_initialized as has_init_mp,
};

pub use core_local::{CloneBootstrap, ConstructMethod, Constructor, CoreLocal};

/// The local APIC for the current core.
pub static LAPIC: Lapic = Lapic::new();

/// The IO APIC for the current system.
pub static IOAPIC: IoApic = IoApic::new();

fn init() -> Result<(), Infallible> {
    LAPIC.init();
    IOAPIC.init();
    info!("IO APIC Version: {:?}", IOAPIC.version());
    let version = IOAPIC.version();
    info!(
        "Max Redirection Entries: {}",
        version.max_redirection_entries()
    );

    // Disable the PICs
    unsafe {
        hardware::disable();
    }
    dispatch_others(load_idt);
    dispatch_all(apic_init);
    core_wait();
    Ok(())
}

fn load_idt() {
    unsafe {
        LGDT.load();
        crate::interrupts::IDT.load();
    }
}

fn apic_init() {
    info!("Initializing LAPIC on core {}", current_core_id());
    info!("LAPIC Version: {:?}", LAPIC.version());
    // So this is *not* a smart way to do this, but considering how many issues i've had with getting IPIs to work, im just going to
    // enable stuff the raw way.

    // First, enable xAPIC mode
    unsafe {
        let mut msr = LAPIC_BASE_MSR.read();
        msr.set_bit(11, true); // Enable xAPIC mode
        let mut base = LAPIC_BASE_MSR;
        base.write(msr);
    }

    // Second, enable the LAPIC and some basic interrupts.
    LAPIC.spurious_interrupt_vector().enable();

    // Third, enable the APIC error interrupt
    let mut error_reg: u32 = 0;
    error_reg.set_bit_range(7, 0, KernelInterrupt::ApicError as u8);
    unsafe { LAPIC.write_offset(0x280, error_reg) };

    // Fourth, set DFR to all ones (flat model) and LDR to logical ID 1
    unsafe {
        LAPIC.write_offset(0xE0, 0xFFFFFFFFu32); // DFR
    }

    // Finally, enable interrupts globally on this core
    interrupts::enable();
}

declare_module!("MP", init);

/// Returns the current core's APIC ID.
#[inline]
pub fn current_core_id() -> u64 {
    // INFO: We don't use `CpuId::new()` because RA fails to generate the IDE definition for it on non-x86_64 platforms.
    CpuId::with_cpuid_reader(raw_cpuid::CpuIdReaderNative)
        .get_feature_info()
        .map_or(0, |finfo| finfo.initial_local_apic_id() as u64)
}

fn apic_page_flags() -> x86_64::structures::paging::PageTableFlags {
    use x86_64::structures::paging::PageTableFlags as Flags;
    Flags::PRESENT | Flags::NO_CACHE | Flags::WRITABLE | Flags::NO_EXECUTE
}

mod _macro {
    /// Defines a constant identifier for a given type.
    macro_rules! id {
        ($typ: ident, $name: ident, $value: expr) => {
            impl $typ {
                /// The register offset for this type.
                pub const $name: usize = $value;
            }
        };
    }

    pub(super) use id;
}

pub(self) use _macro::*;
