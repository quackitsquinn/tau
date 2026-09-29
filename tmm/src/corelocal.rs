use core::cell::UnsafeCell;

use alloc::{boxed::Box, vec::Vec};
use cake::{Once, log::info};

use crate::{MapFlags, paging::heapless::HeaplessAllocator};

/// A core-local storage type that allows for storing data that is specific to each CPU core.
/// This is similar to thread-local storage, but for CPU cores instead of threads.
pub struct CoreLocal<T, I> {
    cores: Once<Box<[UnsafeCell<Option<T>>], HeaplessAllocator>>,
    bootstrap: UnsafeCell<Option<T>>,
    initializer: I,
}

impl<T, I> CoreLocal<T, I>
where
    I: CoreLocalInitializer<T>,
{
    /// Creates a new `CoreLocal` instance with the given initializer.
    pub const fn new(initializer: I) -> Self {
        Self {
            cores: Once::new(),
            bootstrap: UnsafeCell::new(None),
            initializer,
        }
    }

    fn get_cores(&self) -> &[UnsafeCell<Option<T>>] {
        self.cores.call_once(|| {
            // SAFETY: All zeros == None for Option<T>
            let n_cores = cake::core_count().unwrap_or_else(|| {
                info!("Core count not set, defaulting to 1 core");
                1
            });
            let n_aps = n_cores - 1;
            let mut vec = Vec::with_capacity_in(n_aps, HeaplessAllocator::new(MapFlags::WRITABLE));
            for _ in 0..(n_aps) {
                vec.push(UnsafeCell::new(None));
            }
            vec.into_boxed_slice()
        })
    }

    /// Returns a reference to the core-local data for the current core, using the provided initializer.
    /// If the data has not been initialized for the current core, it will be initialized using the provided initializer.
    pub fn get_with<O>(&self, initializer: &O) -> &T
    where
        O: CoreLocalInitializer<T>,
    {
        let core_id = cake::core_id();
        if core_id == 0 {
            // SAFETY: We are the only thread running on the bootstrap core.
            let bootstrap = unsafe { &mut *self.bootstrap.get() };
            if bootstrap.is_none() {
                *bootstrap = Some(initializer.init(core_id as usize));
            }
            bootstrap.as_ref().unwrap()
        } else {
            let cores = self.get_cores();
            let cell = &cores[core_id as usize - 1];
            // SAFETY: We are the only thread running on this core.
            let value = unsafe { &mut *cell.get() };
            if value.is_none() {
                *value = Some(initializer.init(core_id as usize));
            }
            value.as_ref().unwrap()
        }
    }

    /// Returns a reference to the core-local data for the current core, using the provided initializer.
    /// If the data has not been initialized for the current core, it will be initialized using the provided initializer.
    pub fn get_with_inline<O>(&self, initializer: O) -> &T
    where
        O: FnOnce(usize) -> T,
    {
        let core_id = cake::core_id();
        if core_id == 0 {
            // SAFETY: We are the only thread running on the bootstrap core.
            let bootstrap = unsafe { &mut *self.bootstrap.get() };
            if bootstrap.is_none() {
                *bootstrap = Some(initializer(core_id as usize));
            }
            bootstrap.as_ref().unwrap()
        } else {
            let cores = self.get_cores();
            let cell = &cores[core_id as usize - 1];
            // SAFETY: We are the only thread running on this core.
            let value = unsafe { &mut *cell.get() };
            if value.is_none() {
                *value = Some(initializer(core_id as usize));
            }
            value.as_ref().unwrap()
        }
    }
}

impl<T, I> CoreLocal<T, I>
where
    I: HasDefaultInit<T>,
{
    /// Returns a reference to the core-local data for the current core, using the default initializer.
    /// If the data has not been initialized for the current core, it will be initialized using the default initializer.
    pub fn get(&self) -> &T {
        self.get_with(&self.initializer)
    }
}

pub impl(crate) trait CoreLocalInitializer<T> {
    fn init(&self, core_id: usize) -> T;
}

pub impl(crate) trait HasDefaultInit<T>: CoreLocalInitializer<T> {}

pub struct CloneInit<T>
where
    T: Clone,
{
    pub value: T,
}

pub const fn clone_init<T>(value: T) -> CloneInit<T>
where
    T: Clone,
{
    CloneInit { value }
}

impl<T> HasDefaultInit<T> for CloneInit<T> where T: Clone {}

impl<T> CoreLocalInitializer<T> for CloneInit<T>
where
    T: Clone,
{
    fn init(&self, _core_id: usize) -> T {
        self.value.clone()
    }
}

pub struct FnInit<T, F>
where
    F: Fn(usize) -> T,
{
    pub f: F,
}

pub const fn fn_init<T, F>(f: F) -> FnInit<T, F>
where
    F: Fn(usize) -> T,
{
    FnInit { f }
}

impl<T, F> HasDefaultInit<T> for FnInit<T, F> where F: Fn(usize) -> T {}

impl<T, F> CoreLocalInitializer<T> for FnInit<T, F>
where
    F: Fn(usize) -> T,
{
    fn init(&self, core_id: usize) -> T {
        (self.f)(core_id)
    }
}

pub struct CopyInit<T>
where
    T: Copy,
{
    pub value: T,
}

pub const fn copy_init<T>(value: T) -> CopyInit<T>
where
    T: Copy,
{
    CopyInit { value }
}

impl<T> HasDefaultInit<T> for CopyInit<T> where T: Copy {}

impl<T> CoreLocalInitializer<T> for CopyInit<T>
where
    T: Copy,
{
    fn init(&self, _core_id: usize) -> T {
        self.value
    }
}

unsafe impl<T, I: Send> Send for CoreLocal<T, I> {}
unsafe impl<T, I: Sync> Sync for CoreLocal<T, I> {}

pub struct NoDefaultInit;

pub const fn no_default_init() -> NoDefaultInit {
    NoDefaultInit
}

impl<T> CoreLocalInitializer<T> for NoDefaultInit {
    fn init(&self, _core_id: usize) -> T {
        panic!("CoreLocal value not initialized for this core");
    }
}
