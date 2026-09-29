use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(all(target_arch = "x86_64", not(test)))]
use x86_64::registers::model_specific::Msr;

static CURRENT_CORE_ID: AtomicUsize = AtomicUsize::new(0);

/// Initializes the core ID for the current core. This should be called once per core during startup.
///
/// # Safety
/// The caller must ensure that this function is only called once per core, and that the core ID is not already set.
/// Calling this function multiple times on the same core may lead to undefined behavior.
pub unsafe fn init_core_id() -> usize {
    let our_id = CURRENT_CORE_ID.fetch_add(1, Ordering::SeqCst);
    unsafe { write_core_id(our_id) };
    our_id
}

#[cfg(all(target_arch = "x86_64", not(test)))]
const IA32_TSC_AUX_ID: u32 = 0xC0000103;
#[cfg(all(target_arch = "x86_64", not(test)))]
const IA32_TSC_AUX_MSR: Msr = Msr::new(IA32_TSC_AUX_ID);

fn read_core_id() -> usize {
    #[cfg(not(target_arch = "x86_64"))]
    return 0;
    #[cfg(target_arch = "x86_64")]
    return unsafe { IA32_TSC_AUX_MSR.read() } as usize;
}

unsafe fn write_core_id(core_id: usize) {
    #[cfg(not(target_arch = "x86_64"))]
    return;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        #[allow(const_item_mutation)]
        IA32_TSC_AUX_MSR.write(core_id as u64)
    };
}

/// Gets the current core ID.
// TODO: Don't use the initial local APIC ID because it may be reconfigured.. I don't know if CpuId can do tht
#[allow(unreachable_code)]
pub fn core_id() -> u64 {
    return read_core_id() as u64;

    #[cfg(any(test, feature = "std"))]
    return std::thread::current().id().as_u64().into();
}
