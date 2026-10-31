//! Various paging primitives, including pages, frames, and addresses.
//!
pub mod address;
pub mod frame;
pub mod paddr;
pub mod page;
pub mod vaddr;

use core::{
    alloc::Layout,
    mem::{Alignment, transmute_copy},
};

pub use address::{Address, AddressExt};
use cake::encapsulate_macro;
pub use frame::Frame;
pub use paddr::PhysAddr;
pub use page::Page;
pub use vaddr::VirtAddr;

use crate::{align, arch::L1_PAGE_SIZE};

encapsulate_macro!(
    impl_ops,
    _impl_op_mod,
    macro_rules! impl_ops {
    (single $op: tt, $op_trait: ident, $op_fn_name: ident, $newtype: ident ) => {
        const impl ops::$op_trait<u64> for $newtype {
            type Output = Self;
            fn $op_fn_name(self, rhs: u64) -> Self {
                Self(self.0.$op_fn_name(rhs))
            }
        }

        const impl ops::$op_trait<Self> for $newtype {
            type Output = Self;
            fn $op_fn_name(self, rhs: Self) -> Self {
                Self(self.0.$op_fn_name(rhs.0))
            }
        }
    };

    (assign $op: tt, $op_trait: ident, $op_fn_name: ident, $newtype: ident) => {
        const impl ops::$op_trait<u64> for $newtype {
            fn $op_fn_name(&mut self, rhs: u64) {
                self.0.$op_fn_name(rhs);
            }
        }

        const impl ops::$op_trait<Self> for $newtype {
            fn $op_fn_name(&mut self, rhs: Self) {
                self.0.$op_fn_name(rhs.0);
            }
        }
    };

    (blanket $newtype: ident) => {
        impl_ops!(single Add, Add, add, $newtype);
        impl_ops!(single Sub, Sub, sub, $newtype);
        impl_ops!(assign AddAssign, AddAssign, add_assign, $newtype);
        impl_ops!(assign SubAssign, SubAssign, sub_assign, $newtype);
    };
}
);

/// A trait representing a page size for the current architecture.
pub impl(crate) trait FragmentSize:
    Sized + Copy + core::fmt::Debug + Eq + PartialEq
{
    /// The size of this fragment size type, in bytes.
    const SIZE: u64;
    /// The number of bits in this fragment size type.
    const BITS: u64 = Self::SIZE / L1_PAGE_SIZE;
    /// The name of this fragment size type, as a string.
    const NAME: &'static str;
    /// The layout for this fragment size type, used for allocation and deallocation.
    const LAYOUT: Layout = match Layout::from_size_align(Self::SIZE as usize, Self::SIZE as usize) {
        Ok(layout) => layout,
        Err(_) => panic!("Invalid layout"),
    };

    /// The level of the fragment table that this fragment size type corresponds to.
    const LEVEL: u8;

    /// Is this fragment size type considered huge for the current architecture?
    const IS_HUGE: bool = Self::SIZE > L1_PAGE_SIZE;

    /// The alignment of this fragment size type.
    const ALIGNMENT: Alignment = Alignment::new(Self::SIZE as usize).unwrap();
}

/// Marker type for small pages, typically 4KB in size for x86_64 architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Small;
impl FragmentSize for Small {
    const SIZE: u64 = crate::arch::L1_PAGE_SIZE;
    const NAME: &'static str = "Small";
    const LEVEL: u8 = 1;
}
/// Marker type for medium pages, typically 2MB in size for x86_64 architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Medium;
impl FragmentSize for Medium {
    const SIZE: u64 = crate::arch::L2_PAGE_SIZE;
    const NAME: &'static str = "Medium";
    const LEVEL: u8 = 2;
}
/// Marker type for large pages, typically 1GB in size for x86_64 architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Large;
impl FragmentSize for Large {
    const SIZE: u64 = crate::arch::L3_PAGE_SIZE;
    const NAME: &'static str = "Large";
    const LEVEL: u8 = 3;
}

/// A memory primitive.
pub impl(crate) trait Primitive:
    Sized + Copy + core::fmt::Debug + Eq + PartialEq
{
}

/// A trait that represents both Page and Frame types, allowing for generic functions that can work with either type of memory primitive.
pub const trait MemoryFragment<Size: FragmentSize>: Primitive {
    /// The address space type associated with this memory primitive (e.g., `VirtAddr` for pages, `PhysAddr` for frames).
    type AddressType: [const] Address;

    /// Tries to create a new memory primitive from the given starting address.
    /// The address must be aligned to the size of the primitive, otherwise this function will return `None`.
    unsafe fn from_start_address_unchecked(start_address: Self::AddressType) -> Self;

    fn from_start_address(start_address: Self::AddressType) -> Option<Self> {
        if start_address.as_u64() % Size::SIZE == 0 {
            Some(unsafe { Self::from_start_address_unchecked(start_address) })
        } else {
            None
        }
    }

    /// Creates a new memory primitive containing the given address.
    /// The starting address of the primitive will be the largest aligned address less than or equal to the given address.
    fn containing_address(addr: Self::AddressType) -> Self;

    /// Returns the starting address of this memory primitive as the appropriate address space type (e.g., `VirtAddr` for pages, `PhysAddr` for frames).
    fn start_address(&self) -> Self::AddressType;
}

/// A trait representing a family of memory primitives (e.g., pages, frames, and virt/phys addresses) that can be used in paging,
pub impl(crate) const trait PrimitiveClass:
    Sized + Copy + core::fmt::Debug + Eq + PartialEq
{
    /// The address space type associated with this family of memory fragments (e.g., `VirtAddr` for pages, `PhysAddr` for frames).
    type Addr: Address;

    /// The memory fragments type associated with this family of memory fragments (e.g., `Page<S>` for pages, `Frame<S>` for frames).
    type Fragment<S: FragmentSize>: MemoryFragment<S, AddressType = Self::Addr>;
}

/// The primitives used for virtual addresses and pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PageClass;
impl PrimitiveClass for PageClass {
    type Addr = VirtAddr;
    type Fragment<S: FragmentSize> = Page<S>;
}

/// The primitives used for physical addresses and frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameClass;
impl PrimitiveClass for FrameClass {
    type Addr = PhysAddr;
    type Fragment<S: FragmentSize> = Frame<S>;
}

/// A memory primitive of unknown size.
/// This is used for functions that need to work with memory primitives of any size, but don't need to know the specific size of the primitive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnyFragment<C>
where
    C: PrimitiveClass,
{
    /// A small memory primitive, typically 4KB in size for x86_64 architecture.
    Small(C::Fragment<Small>),
    /// A medium memory primitive, typically 2MB in size for x86_64 architecture.
    Medium(C::Fragment<Medium>),
    /// A large memory primitive, typically 1GB in size for x86_64 architecture.
    Large(C::Fragment<Large>),
}

impl<C> AnyFragment<C>
where
    C: PrimitiveClass,
{
    /// Returns the starting address of this memory primitive as the appropriate address space type (e.g., `VirtAddr` for pages, `PhysAddr` for frames).
    pub fn start_address(&self) -> C::Addr {
        match self {
            AnyFragment::Small(prim) => prim.start_address(),
            AnyFragment::Medium(prim) => prim.start_address(),
            AnyFragment::Large(prim) => prim.start_address(),
        }
    }

    /// Returns the size of this memory primitive in bytes.
    pub fn size(&self) -> u64 {
        match self {
            AnyFragment::Small(_) => Small::SIZE,
            AnyFragment::Medium(_) => Medium::SIZE,
            AnyFragment::Large(_) => Large::SIZE,
        }
    }

    /// Returns Self as C::Fragment<S> if S::SIZE is less than or equal to the size of self.
    pub fn downsize_as<S: FragmentSize>(self) -> C::Fragment<S> {
        // SAFETY: C::Fragment<S> is the same type as C::Fragment<...>
        match self {
            AnyFragment::Small(p) => {
                return unsafe { transmute_copy(&p) };
            }
            AnyFragment::Medium(p) if S::SIZE <= Medium::SIZE => {
                return unsafe { transmute_copy(&p) };
            }
            AnyFragment::Large(p) if S::SIZE == Large::SIZE => {
                return unsafe { transmute_copy(&p) };
            }
            _ => {
                panic!(
                    "AnyPrimitive::downsize_as: Fragment {:?} is too large for size {}",
                    self,
                    S::NAME
                )
            }
        }
    }

    pub fn unwrap_as<S: FragmentSize>(self) -> C::Fragment<S> {
        // SAFETY: C::Fragment<S> is the same type as C::Fragment<_>
        match self {
            AnyFragment::Small(p) if S::SIZE == Small::SIZE => {
                return unsafe { transmute_copy(&p) };
            }
            AnyFragment::Medium(p) if S::SIZE == Medium::SIZE => {
                return unsafe { transmute_copy(&p) };
            }
            AnyFragment::Large(p) if S::SIZE == Large::SIZE => {
                return unsafe { transmute_copy(&p) };
            }
            _ => {
                panic!(
                    "AnyPrimitive::unwrap_as: Found {:?}, expected {:?}",
                    self,
                    S::NAME
                )
            }
        }
    }

    /// Dissects the AnyFragment into its specific size variant and calls the appropriate closure with the contained fragment and additional parameters.
    pub fn dissect_with<Lf, Mf, Sf, P, R>(self, lf: Lf, mf: Mf, sf: Sf, params: P) -> R
    where
        Sf: FnOnce(C::Fragment<Small>, P) -> R,
        Mf: FnOnce(C::Fragment<Medium>, P) -> R,
        Lf: FnOnce(C::Fragment<Large>, P) -> R,
    {
        match self {
            AnyFragment::Small(page) => sf(page, params),
            AnyFragment::Medium(page) => mf(page, params),
            AnyFragment::Large(page) => lf(page, params),
        }
    }
}

/// Type alias for a memory primitive of unknown size that is specifically a page.
pub type AnyPage = AnyFragment<PageClass>;
/// Type alias for a memory primitive of unknown size that is specifically a frame.
pub type AnyFrame = AnyFragment<FrameClass>;

/// A range of memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryRange<A>
where
    A: Address,
{
    start: A,
    end: A,
}

impl<A> MemoryRange<A>
where
    A: const Address,
{
    /// Creates a new MemoryRange from the given start and end addresses.
    #[track_caller]
    pub const fn new(start: A, end: A) -> Self {
        Self::try_new(start, end).expect("MemoryRange::new: `start` is greater than `end`")
    }

    /// Attempts to create a new MemoryRange from the given start and end addresses.
    /// Returns `None` if the start address is greater than the end address
    pub const fn try_new(start: A, end: A) -> Option<Self> {
        if start < end {
            Some(Self { start, end })
        } else {
            None
        }
    }

    /// Creates a new MemoryRange from the given start address and length in bytes.
    /// Returns `None` if the resulting end address would overflow.
    pub const fn try_new_len(start: A, length: u64) -> Option<Self> {
        let end = A::try_new(start.as_u64() + length);
        match end {
            Some(end) => Self::try_new(start, end),
            None => None,
        }
    }

    /// Creates a new MemoryRange from the given start address and length in bytes.
    /// Panics if the resulting end address would overflow.
    pub const fn new_len(start: A, length: u64) -> Self {
        Self::try_new_len(start, length).expect("MemoryRange::new_len: `start + length` overflowed")
    }

    /// Returns the starting address of this memory range.
    pub const fn start(&self) -> A {
        self.start
    }

    /// Returns the ending address of this memory range.
    pub const fn end(&self) -> A {
        self.end
    }

    /// Returns the starting address of this memory range.
    pub const fn size(&self) -> u64 {
        self.end.as_u64() - self.start.as_u64()
    }

    /// Returns the alignment of the start address of this range
    pub const fn start_align(&self) -> Alignment {
        match self.start.as_u64().lowest_one() {
            Some(z) => return Alignment::new(1 << z as u64).unwrap(),
            None => return Alignment::MIN,
        }
    }

    /// Returns a new MemoryRange that is aligned up to the next multiple of the given fragment size.
    /// Returns `None` if the aligned start address is greater than or equal to the end address.
    pub const fn align_up_to<S: FragmentSize>(&self) -> Option<MemoryRange<A>> {
        let aligned_start = match A::try_new(align!(up, self.start.as_u64(), S::SIZE)) {
            Some(addr) => addr,
            None => return None,
        };

        if aligned_start >= self.end {
            return None;
        }

        Some(MemoryRange {
            start: aligned_start,
            end: self.end,
        })
    }

    /// Truncates the memory range to the given new size, keeping the start address the same.
    /// Panics if the new size is greater than the current size of the range.
    pub fn truncate(mut self, new_size: u64) -> Self {
        let new_end = self.start.as_u64() + new_size;
        if new_end > self.end.as_u64() {
            panic!(
                "MemoryRange::truncate: `start + new_size` ({:#x}) is greater than `end` ({:#x})",
                new_end,
                self.end.as_u64()
            );
        }
        self.end =
            A::try_new(new_end).expect("MemoryRange::truncate: `start + new_size` overflowed");
        self
    }

    pub fn align_barriers(self, align: Alignment) -> Self {
        let start = A::try_new(align!(down, self.start.as_u64(), align.as_usize() as u64)).unwrap();
        let end = A::try_new(align!(up, self.end.as_u64(), align.as_usize() as u64)).unwrap();
        MemoryRange { start, end }
    }
}

/// A range of physical memory.
pub type PhysRange = MemoryRange<PhysAddr>;
/// A range of virtual memory.
pub type VirtRange = MemoryRange<VirtAddr>;

/// A direct mapping between a page and a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectMapping {
    /// A mapping between a small page and a small frame.
    Small(Page<Small>, Frame<Small>),
    /// A mapping between a medium page and a medium frame.
    Medium(Page<Medium>, Frame<Medium>),
    /// A mapping between a large page and a large frame.
    Large(Page<Large>, Frame<Large>),
}

impl DirectMapping {
    /// Returns the page associated with this mapping.
    pub fn page(&self) -> AnyFragment<PageClass> {
        match self {
            DirectMapping::Small(page, _) => AnyFragment::Small(*page),
            DirectMapping::Medium(page, _) => AnyFragment::Medium(*page),
            DirectMapping::Large(page, _) => AnyFragment::Large(*page),
        }
    }

    /// Returns the frame associated with this mapping.
    pub fn frame(&self) -> AnyFragment<FrameClass> {
        match self {
            DirectMapping::Small(_, frame) => AnyFragment::Small(*frame),
            DirectMapping::Medium(_, frame) => AnyFragment::Medium(*frame),
            DirectMapping::Large(_, frame) => AnyFragment::Large(*frame),
        }
    }

    /// Returns the size of this mapping in bytes.
    pub fn size(&self) -> u64 {
        match self {
            DirectMapping::Small(_, _) => Small::SIZE,
            DirectMapping::Medium(_, _) => Medium::SIZE,
            DirectMapping::Large(_, _) => Large::SIZE,
        }
    }

    /// Creates a new DirectMapping from the given page and frame.
    pub fn new<S: FragmentSize>(page: Page<S>, frame: Frame<S>) -> Self {
        match S::SIZE {
            Small::SIZE => DirectMapping::Small(unsafe { transmute_copy(&page) }, unsafe {
                transmute_copy(&frame)
            }),
            Medium::SIZE => DirectMapping::Medium(unsafe { transmute_copy(&page) }, unsafe {
                transmute_copy(&frame)
            }),
            Large::SIZE => DirectMapping::Large(unsafe { transmute_copy(&page) }, unsafe {
                transmute_copy(&frame)
            }),
            _ => panic!("DirectMapping::new: Invalid fragment size"),
        }
    }
}
