//! Virtual address type for the current architecture.
use crate::{
    arch,
    paging::{
        Address,
        primitives::{Primitive, impl_ops},
    },
};

use core::ops;

/// Virtual address type for the current architecture.
#[derive(Clone, Copy, Hash, Debug)]
#[derive_const(PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtAddr(u64);

impl_ops!(blanket VirtAddr);

impl VirtAddr {
    /// The start of the higher half in virtual address space.
    pub const HIGHER_HALF_OFFSET: Self = arch::HIGHER_HALF_START;

    /// Returns true if the virtual address is null (0).
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }
}

impl Primitive for VirtAddr {}

const impl Address for VirtAddr {
    fn try_new(val: u64) -> Option<Self> {
        if arch::is_valid_virt(val) {
            Some(VirtAddr(val))
        } else {
            None
        }
    }

    unsafe fn new_unchecked(val: u64) -> Self {
        VirtAddr(val)
    }

    fn new_truncate(val: u64) -> Self {
        Self(arch::canonicalize_virt(val))
    }

    fn as_u64(&self) -> u64 {
        self.0
    }
}
