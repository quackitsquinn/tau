use core::{cell::UnsafeCell, fmt};

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

impl<T, I> fmt::Debug for CoreLocal<T, I>
where
    T: fmt::Debug,
    I: CoreLocalInitializer<T>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreLocal")
            .field("current", &self.try_get())
            .finish()
    }
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

    /// Returns an `Option` containing a reference to the core-local data for the current core, if it has been initialized.
    /// If the data has not been initialized for the current core, it will return `None`.`
    pub fn try_get(&self) -> Option<&T> {
        let core_id = cake::core_id();
        if core_id == 0 {
            // SAFETY: We are the only thread running on the bootstrap core.
            let bootstrap = unsafe { &mut *self.bootstrap.get() };
            bootstrap.as_ref()
        } else {
            let cores = self.get_cores();
            let cell = &cores[core_id as usize - 1];
            // SAFETY: We are the only thread running on this core.
            let value = unsafe { &mut *cell.get() };
            value.as_ref()
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

/// A trait for types that can initialize core-local data. This trait is used to provide a way to initialize core-local data for each core.
pub impl(crate) trait CoreLocalInitializer<T> {
    /// Initializes the core-local data for the current core.
    ///  This method is called when the core-local data is first accessed on a core that has not yet initialized its data.
    fn init(&self, core_id: usize) -> T;
}

/// A trait for types that can provide a default initializer for core-local data. This trait is used to provide a way to initialize core-local data for each core with a default value.
pub impl(crate) trait HasDefaultInit<T>: CoreLocalInitializer<T> {}

/// A core-local initializer that clones a value for each core. This is useful for types that implement the `Clone` trait and can be initialized with a default value.
#[derive(Debug)]
pub struct CloneInit<T>
where
    T: Clone,
{
    /// The value to be cloned for each core.
    pub value: T,
}

/// Creates a new `CloneInit` with the given value. This value will be cloned for each core when initializing core-local data.
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

/// A core-local initializer that uses a function to initialize the data for each core.
/// This is useful for types that require more complex initialization logic.
#[derive(Debug)]
pub struct FnInit<T, F>
where
    F: Fn(usize) -> T,
{
    /// The function to be called to initialize the data for each core.
    pub f: F,
}

/// Creates a new `FnInit` with the given function. This function will be called for each core to initialize the core-local data.
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

/// A core-local initializer that copies a value for each core. This is useful for types that
/// implement the `Copy` trait and can be initialized with a default value.
#[derive(Debug)]
pub struct CopyInit<T>
where
    T: Copy,
{
    /// The value to be copied for each core.
    pub value: T,
}

/// Creates a new `CopyInit` with the given value. This value will be copied for each core when initializing core-local data.
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

/// A core-local initializer that does not provide a default initializer. This is useful for types that require explicit initialization for each core.
#[derive(Debug)]
pub struct NoDefaultInit;

/// Creates a new `NoDefaultInit`. This initializer does not provide a default value for core-local data.
pub const fn no_default_init() -> NoDefaultInit {
    NoDefaultInit
}

impl<T> CoreLocalInitializer<T> for NoDefaultInit {
    fn init(&self, _core_id: usize) -> T {
        panic!("CoreLocal value not initialized for this core");
    }
}
