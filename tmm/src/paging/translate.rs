//! Address translation trait

use cake::log::error;

use crate::{
    MapFlags, MemError,
    err::MalformedPageStructure,
    paging::{
        FragmentSize, Frame, MemoryFragment, MemoryRange, Page, PageTableIndex, Small, VirtAddr,
        accessor::{self, PagetableAccessor},
        primitives::DirectMapping,
    },
};

/// A trait for translating virtual addresses to physical addresses in a given address space.
pub trait Translate {
    /// Translates a virtual address to a physical address in the context of this address space.
    /// Returns a `TranslateResult` indicating the outcome of the translation.
    fn translate(&self, addr: VirtAddr) -> TranslateResult;
    /// Returns an iterator over the present mappings in the given range of virtual addresses.
    fn present_mappings(&self, range: MemoryRange<VirtAddr>) -> PresentRangeIterator<'_, Self> {
        PresentRangeIterator::new(self, range)
    }
}

/// The result of a translation attempt, indicating whether the translation was successful, not mapped, or resulted in an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateResult {
    /// The translation was successful, and the resulting physical address is contained in the `AnyFragment<FrameClass>`.
    Success(DirectMapping, MapFlags),
    /// The virtual address is not mapped in this address space.
    /// `.0` contains the level of the page table where the mapping was not found (4 for l4, 3 for l3, etc..).
    NotMapped(u8),
    /// An error occurred during the translation process, such as an invalid address or a failure to access the page tables.
    Error(MemError),
}

impl From<MemError> for TranslateResult {
    fn from(err: MemError) -> Self {
        TranslateResult::Error(err)
    }
}

impl<T> Translate for T
where
    T: PagetableAccessor,
{
    fn translate(&self, addr: VirtAddr) -> TranslateResult {
        let (l4, l3, l2, l1) = accessor::dissolve_address(addr);
        let l3_table = match self.l3_table(l4) {
            Ok(table) => table,
            Err(MemError::MalformedPageStructure(MalformedPageStructure::NotMapped(_))) => {
                return TranslateResult::NotMapped(4);
            }
            Err(e) => return TranslateResult::Error(e),
        };

        let l3_ent = l3_table.read_entry(l3);
        if !l3_ent.is_present() {
            return TranslateResult::NotMapped(3);
        } else if l3_ent.is_huge() {
            let frame = Frame::from_start_address(l3_ent.addr()).unwrap();
            let page = Page::from_start_address(accessor::build_vaddress(
                l4,
                l3,
                PageTableIndex::MIN,
                PageTableIndex::MIN,
            ))
            .unwrap();
            return TranslateResult::Success(DirectMapping::Large(page, frame), l3_ent.flags());
        }

        let l2_table = match self.l2_table(l4, l3) {
            Ok(table) => table,
            Err(e) => return TranslateResult::Error(e),
        };

        let l2_ent = l2_table.read_entry(l2);
        if !l2_ent.is_present() {
            return TranslateResult::NotMapped(2);
        } else if l2_ent.is_huge() {
            let frame = Frame::from_start_address(l2_ent.addr()).unwrap();
            let page =
                Page::from_start_address(accessor::build_vaddress(l4, l3, l2, PageTableIndex::MIN))
                    .unwrap();
            return TranslateResult::Success(DirectMapping::Medium(page, frame), l2_ent.flags());
        }

        let l1_table = match self.l1_table(l4, l3, l2) {
            Ok(table) => table,
            Err(e) => return TranslateResult::Error(e),
        };

        let l1_ent = l1_table.read_entry(l1);
        if !l1_ent.is_present() {
            return TranslateResult::NotMapped(1);
        } else if l1_ent.is_huge() {
            return TranslateResult::Error(MemError::InvalidOperation);
        }

        let frame = Frame::from_start_address(l1_ent.addr()).unwrap();
        let page = Page::from_start_address(accessor::build_vaddress(l4, l3, l2, l1)).unwrap();
        TranslateResult::Success(DirectMapping::Small(page, frame), l1_ent.flags())
    }
}

/// An iterator over the present mappings in a given range of virtual addresses.
#[derive(Debug)]
pub struct PresentRangeIterator<'a, T: Translate + ?Sized> {
    table: &'a T,
    range: MemoryRange<VirtAddr>,
    current: VirtAddr,
}

impl<'a, T: Translate + ?Sized> PresentRangeIterator<'a, T> {
    /// Creates a new iterator over the present mappings in the given range.
    pub fn new(table: &'a T, range: MemoryRange<VirtAddr>) -> Self {
        Self {
            table,
            range,
            current: range.start(),
        }
    }
}

impl<'a, T: Translate + ?Sized> Iterator for PresentRangeIterator<'a, T> {
    type Item = (DirectMapping, MapFlags);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current >= self.range.end() {
            return None;
        }

        while self.current < self.range.end() {
            match self.table.translate(self.current) {
                TranslateResult::Success(mapping, flags) => {
                    self.current += mapping.size() as u64; // Move to the next page after the current mapping
                    return Some((mapping, flags));
                }
                TranslateResult::NotMapped(lvl) => {
                    // Move to the next page and try again
                    match lvl {
                        4 => {
                            self.current += accessor::build_address(
                                PageTableIndex::new(1),
                                PageTableIndex::MIN,
                                PageTableIndex::MIN,
                                PageTableIndex::MIN,
                            )
                        }
                        3 => {
                            self.current += accessor::build_address(
                                PageTableIndex::MIN,
                                PageTableIndex::new(1),
                                PageTableIndex::MIN,
                                PageTableIndex::MIN,
                            )
                        }
                        2 => {
                            self.current += accessor::build_address(
                                PageTableIndex::MIN,
                                PageTableIndex::MIN,
                                PageTableIndex::new(1),
                                PageTableIndex::MIN,
                            )
                        }
                        1 => {
                            self.current += accessor::build_address(
                                PageTableIndex::MIN,
                                PageTableIndex::MIN,
                                PageTableIndex::MIN,
                                PageTableIndex::new(1),
                            )
                        }
                        _ => unreachable!(),
                    }
                }
                TranslateResult::Error(e) => {
                    error!("Error while translating address: {:?}", e);
                    return None;
                }
            }
        }

        None
    }
}
