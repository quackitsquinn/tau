use alloc::alloc::alloc;
use cake::RwLockUpgradableReadGuard;
use core::sync::atomic::AtomicUsize;
use core::{alloc::Layout, arch::naked_asm, hint, sync::atomic::Ordering};
use x86_64::registers::control::{Cr3, Cr3Flags};

use cake::log::info;
use cake::{RwLock, limine::mp::Cpu};

use crate::mp::mp_setup::CORE_COUNT;
use crate::mp::mp_setup::CoreContext;

#[unsafe(naked)]
pub unsafe extern "C" fn _ap_trampoline(a: &Cpu) -> ! {
    naked_asm!(
        "mov rsi, rsp", // Pass rsp as the second argument (first is cpu pointer)
        "call {ap_trampoline}",
        ap_trampoline = sym ap_trampoline,
    )
}

static IDLE: AtomicUsize = AtomicUsize::new(0);
static INIT: AtomicUsize = AtomicUsize::new(0);

// Application Processor trampoline function.
extern "C" fn ap_trampoline(cpu: &Cpu, stack_base: u64) -> ! {
    let context = unsafe {
        &*(cpu.extra.load(core::sync::atomic::Ordering::SeqCst) as *const RwLock<CoreContext>)
    };

    let context_lock = context.write();

    context_lock.stack_start.call_once(|| stack_base);

    info!("CPU {} (APIC ID {}) started", cpu.id, cpu.lapic_id);
    info!("Stack base: {:#x}", stack_base);
    drop(context_lock);

    unsafe {
        cake::init_core_id();
    }

    // Now we can enter the idle loop.
    IDLE.fetch_add(1, Ordering::AcqRel);
    INIT.fetch_add(1, Ordering::AcqRel);
    loop {
        let context_lock = context.upgradable_read();
        if context_lock.tasks.is_empty() {
            drop(context_lock);
            hint::spin_loop();
            continue;
        }

        IDLE.fetch_sub(1, Ordering::AcqRel);
        let mut context_lock = RwLockUpgradableReadGuard::upgrade(context_lock);
        let task = context_lock.tasks.remove(0);
        task();
        IDLE.fetch_add(1, Ordering::AcqRel);
    }
}

pub(super) fn prepare_cpu(cpu: &Cpu) -> *const RwLock<CoreContext> {
    // First, allocate a context for the CPU.
    let context =
        unsafe { alloc(Layout::new::<RwLock<CoreContext>>()) } as *mut RwLock<CoreContext>;
    unsafe {
        context.write(RwLock::new(CoreContext::new(cpu)));
    }

    // Set it to the CPU's extra field and insert it into the global map.
    cpu.extra.store(context as u64, Ordering::SeqCst);

    cpu.goto_address.write(_ap_trampoline);

    context
}

/// Returns true if all application processors have initialized.
pub fn aps_init() -> bool {
    let total = *CORE_COUNT.get().expect("not all cores init");
    let init = INIT.load(Ordering::Acquire);
    total > 0 && total == init
}

/// Returns true if all application processors have finished their tasks.
pub fn aps_finished() -> bool {
    let total = *CORE_COUNT.get().expect("not all cores init");
    if total == 1 {
        return true;
    }
    let idle = IDLE.load(Ordering::Acquire);
    total - 1 == idle
}

/// Waits until all application processors have finished their tasks.
pub fn core_wait() {
    while !aps_finished() {
        hint::spin_loop();
    }
}
