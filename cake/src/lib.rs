//! Workspace-wide types and utilities
#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, feature(thread_id_value))]
#![feature(debug_closure_helpers)]

mod fuse;
pub mod id;
mod limine_request;
mod module;
mod oncemut;
mod oncerw;
mod owned;
mod resource;
pub mod securand;
pub mod stacktrace;
pub use id::{core_count, core_id, init_core_id, set_core_count};

/* Crate Exports */
pub use self::limine_request::{
    LimineData, LimineRequest, RawLimineRequest, requests_terminated, terminate_requests,
};
pub use fuse::Fuse;
use kelp::Elf;
pub use limine;
pub use lock_api;
pub use log;
pub use module::KernelModule;
pub use oncemut::{OnceMutex, OnceMutexGuard};
pub use oncerw::{OnceRwLock, OnceRwReadGuard, OnceRwWriteGuard};
pub use owned::Owned;
pub use resource::{ResourceGuard, ResourceMutex};
pub use securand::{add_entropy_source, init_rng, rng, rng32, rng64};
pub use spin::lock_api::*;
pub use spin::{Barrier, Lazy, Once};

#[doc(hidden)]
pub use kproc as _macro_internal_kproc;
/// A type alias for a mapped mutex guard.
pub type MappedMutexGuard<'a, T> = lock_api::MappedMutexGuard<'a, RawMutex, T>;

/// Type aliases for a raw mutex.
pub type RawMutex = spin::Mutex<()>;
/// Type alias for a raw read-write lock.
pub type RawRwLock = spin::RwLock<()>;

static CALLER_INSTRUCTION_POINTER_FN: Once<fn() -> usize> = Once::new();
static CALLER_INSTRUCTION_POINTER_NAME_RESOLVER: Once<fn(usize) -> Option<&'static str>> =
    Once::new();
static MULTITHREADED: Once<bool> = Once::new();

pub(crate) static KERNEL_ELF: Once<Elf<'static>> = Once::new();

pub fn set_kernel_elf(kernel_elf: &'static [u8]) -> Result<Option<()>, kelp::ElfError> {
    let elf = Elf::new(kernel_elf)?;
    let mut set = false;
    KERNEL_ELF.call_once(|| {
        set = true;
        elf
    });
    if set { Ok(Some(())) } else { Ok(None) }
}

/// Returns true if the kernel is running in a multithreaded environment (i.e., with multiple cores).
/// This is set automatically to single-threaded until this function is called.
pub fn set_multithreaded(multithreaded: bool) {
    MULTITHREADED.call_once(|| multithreaded);
}

/// Returns true if the kernel is running in a multithreaded environment (i.e., with multiple cores).
pub(crate) fn is_multithreaded() -> bool {
    *MULTITHREADED.get().unwrap_or(&false)
}

/// Encapsulates a macro definition within a private module to prevent it from being used outside of the intended scope.
///
/// # Usage
/// ```
/// encapsulate_macro!(
///     pub(crate) my_macro,
///     _my_macro_mod,
///     macro_rules! my_macro {
///         // macro definition here
///     }
/// );
/// ```
#[macro_export]
macro_rules! encapsulate_macro {
    ($vis: vis $name: ident, $mod_name:ident, $($rest:tt)*) => {
        mod $mod_name {
            $($rest)*
            pub(super) use $name;
        }
        $vis use $mod_name::$name;
    };
}

/// A macro to log a trace message only if tracing is enabled for the given target.
#[macro_export]
macro_rules! trace_disabled {
    ($target: literal, $($rest:tt)*) => {
        const TRACE: bool = $crate::_macro_internal_kproc::should_trace_target!($target);

        if TRACE {
            $crate::log::trace!(target: $target, $($rest)*);
        }
    };
}

#[cfg(test)]
mod test_log {
    use ctor::ctor;

    #[ctor]
    fn log_setup() {
        let _ = env_logger::builder().is_test(true).try_init();
    }
}
