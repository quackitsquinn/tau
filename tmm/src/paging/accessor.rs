//! A trait for accessing page tables at different levels in the page table hierarchy.

use arrayvec::ArrayVec;
use cake::trace_disabled;

use crate::{
    MapFlags,
    MemError::{self},
    align, arch, mem_err,
    paging::{
        Address, AddressExt, FragmentManager, FragmentSize, Frame, FullManager, Large, Medium,
        MemoryFragment, MemoryRange, Page, PageTable, PageTableEntry, PageTableIndex, PhysAddr,
        Small, VirtAddr,
        index::PageIndexIter,
        map::{Flush, GlobalMemoryProvider, MemoryMapper, SizedMemoryMapper},
        primitives::{AnyPage, DirectMapping, FrameClass},
        translate::{Translate, TranslateResult},
    },
};

/// A trait for accessing page tables at different levels in the page table hierarchy.
pub trait PagetableAccessor {
    /// Reads the level 4 page table for the current architecture.
    fn read_l4_table(&self) -> Result<VirtAddr, MemError>;
    /// Reads the level 3 page table for the given index of the higher-level page table.
    fn read_l3_table(&self, l4_index: PageTableIndex) -> Result<VirtAddr, MemError>;
    /// Reads the level 2 page table for the given indices of the higher-level page tables.
    fn read_l2_table(
        &self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
    ) -> Result<VirtAddr, MemError>;
    /// Reads the level 1 page table for the given indices of the higher-level page tables.
    fn read_l1_table(
        &self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
        l2_index: PageTableIndex,
    ) -> Result<VirtAddr, MemError>;

    /// Reads the page table specified by the given `TablePointer`.
    fn read_table(&self, table_ptr: &TablePointer) -> Result<VirtAddr, MemError> {
        match table_ptr.level() {
            3 => self.read_l3_table(dissolve_address(table_ptr.address()).0),
            2 => {
                let (l4, l3, _, _) = dissolve_address(table_ptr.address());
                self.read_l2_table(l4, l3)
            }
            1 => {
                let (l4, l3, l2, _) = dissolve_address(table_ptr.address());
                self.read_l1_table(l4, l3, l2)
            }
            _ => unreachable!("Invalid table level: {}", table_ptr.level()),
        }
    }

    /// Returns a reference to the level 4 page table.
    fn l4_table(&self) -> Result<&PageTable, MemError> {
        let addr = self.read_l4_table()?;
        let table: &PageTable = unsafe { &*(addr.as_ptr()) };
        Ok(table)
    }

    /// Returns a reference to the level 3 page table for the given index of the higher-level page table.
    fn l3_table(&self, l4_index: PageTableIndex) -> Result<&PageTable, MemError> {
        let addr = self.read_l3_table(l4_index)?;
        let table: &PageTable = unsafe { &*(addr.as_ptr()) };
        Ok(table)
    }

    /// Returns a reference to the level 2 page table for the given indices of the higher-level page tables.
    fn l2_table(
        &self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
    ) -> Result<&PageTable, MemError> {
        let addr = self.read_l2_table(l4_index, l3_index)?;
        let table: &PageTable = unsafe { &*(addr.as_ptr()) };
        Ok(table)
    }

    /// Returns a reference to the level 1 page table for the given indices of the higher-level page tables.
    fn l1_table(
        &self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
        l2_index: PageTableIndex,
    ) -> Result<&PageTable, MemError> {
        let addr = self.read_l1_table(l4_index, l3_index, l2_index)?;
        let table: &PageTable = unsafe { &*(addr.as_ptr()) };
        Ok(table)
    }

    /// Returns a mutable reference to the level 4 page table.
    fn l4_table_mut(&mut self) -> Result<&mut PageTable, MemError> {
        let addr = self.read_l4_table()?;
        let table: &mut PageTable = unsafe { &mut *(addr.as_mut_ptr()) };
        Ok(table)
    }

    /// Returns a mutable reference to the level 3 page table for the given index of the higher-level page table.
    fn l3_table_mut(&mut self, l4_index: PageTableIndex) -> Result<&mut PageTable, MemError> {
        let addr = self.read_l3_table(l4_index)?;
        let table: &mut PageTable = unsafe { &mut *(addr.as_mut_ptr()) };
        Ok(table)
    }

    /// Returns a mutable reference to the level 2 page table for the given indices of the higher-level page tables.
    fn l2_table_mut(
        &mut self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
    ) -> Result<&mut PageTable, MemError> {
        let addr = self.read_l2_table(l4_index, l3_index)?;
        let table: &mut PageTable = unsafe { &mut *(addr.as_mut_ptr()) };
        Ok(table)
    }

    /// Returns a mutable reference to the level 1 page table for the given indices of the higher-level page tables.
    fn l1_table_mut(
        &mut self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
        l2_index: PageTableIndex,
    ) -> Result<&mut PageTable, MemError> {
        let addr = self.read_l1_table(l4_index, l3_index, l2_index)?;
        let table: &mut PageTable = unsafe { &mut *(addr.as_mut_ptr()) };
        Ok(table)
    }
}

/// A pointer to a page table at a specific level in the page table hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TablePointer {
    table: VirtAddr,
}

impl TablePointer {
    fn new(
        l4_idx: PageTableIndex,
        l3_idx: PageTableIndex,
        l2_idx: PageTableIndex,
        level: u8,
    ) -> Self {
        let table = VirtAddr::new(
            build_address(l4_idx, l3_idx, l2_idx, PageTableIndex::new(0)) | level as u64,
        );
        Self { table }
    }

    /// Creates a new `TablePointer` for a level 4 page table, given the index for the level 4 page table.
    pub fn new_l3(l4_idx: PageTableIndex) -> Self {
        Self::new(l4_idx, PageTableIndex::new(0), PageTableIndex::new(0), 3)
    }

    /// Creates a new `TablePointer` for a level 2 page table, given the indices for the level 4 and level 3 page tables.
    pub fn new_l2(l4_idx: PageTableIndex, l3_idx: PageTableIndex) -> Self {
        Self::new(l4_idx, l3_idx, PageTableIndex::new(0), 2)
    }

    /// Creates a new `TablePointer` for a level 1 page table, given the indices for the level 4, level 3, and level 2 page tables.
    pub fn new_l1(l4_idx: PageTableIndex, l3_idx: PageTableIndex, l2_idx: PageTableIndex) -> Self {
        Self::new(l4_idx, l3_idx, l2_idx, 1)
    }

    /// Returns the level of the page table this pointer points to (1, 2, 3, or 4).
    pub fn level(&self) -> u8 {
        (self.table.as_u64() & 0x3) as u8
    }

    /// Returns the virtual address pointing up to the page table at the specified level.
    pub fn address(&self) -> VirtAddr {
        VirtAddr::new(self.table.as_u64() & !0x3)
    }

    /// Dissolves the virtual address of the page table into its constituent indices for each level of the page table hierarchy.
    pub fn dissolve(&self) -> (PageTableIndex, PageTableIndex, PageTableIndex) {
        let (l4_idx, l3_idx, l2_idx, _) = dissolve_address(self.address());
        (l4_idx, l3_idx, l2_idx)
    }
}

/// Builds a virtual address from the given page table indices for each level of the page table hierarchy.
pub const fn build_address(
    l4_idx: PageTableIndex,
    l3_idx: PageTableIndex,
    l2_idx: PageTableIndex,
    l1_idx: PageTableIndex,
) -> u64 {
    let mut addr: u64 = 0;
    addr |= (l4_idx.value() as u64) << (arch::TABLE_INDEX_BITS * 3 + arch::ENTRY_OFFSET_BITS);
    addr |= (l3_idx.value() as u64) << (arch::TABLE_INDEX_BITS * 2 + arch::ENTRY_OFFSET_BITS);
    addr |= (l2_idx.value() as u64) << (arch::TABLE_INDEX_BITS + arch::ENTRY_OFFSET_BITS);
    addr |= (l1_idx.value() as u64) << arch::ENTRY_OFFSET_BITS;
    addr
}

/// Builds a virtual address from the given page table indices for each level of the page table hierarchy.
pub const fn build_vaddress(
    l4_idx: PageTableIndex,
    l3_idx: PageTableIndex,
    l2_idx: PageTableIndex,
    l1_idx: PageTableIndex,
) -> VirtAddr {
    VirtAddr::new_truncate(build_address(l4_idx, l3_idx, l2_idx, l1_idx))
}

/// Dissolves a virtual address into its constituent page table indices for each level of the page table hierarchy.
///
/// This does not account for larger sized pages.
pub const fn dissolve_address(
    addr: VirtAddr,
) -> (
    PageTableIndex,
    PageTableIndex,
    PageTableIndex,
    PageTableIndex,
) {
    let addr_val = addr.as_u64();
    let l4_idx = PageTableIndex::new(
        ((addr_val >> (arch::TABLE_INDEX_BITS * 3 + arch::ENTRY_OFFSET_BITS))
            & ((1 << arch::TABLE_INDEX_BITS) - 1)) as u16,
    );
    let l3_idx = PageTableIndex::new(
        ((addr_val >> (arch::TABLE_INDEX_BITS * 2 + arch::ENTRY_OFFSET_BITS))
            & ((1 << arch::TABLE_INDEX_BITS) - 1)) as u16,
    );
    let l2_idx = PageTableIndex::new(
        ((addr_val >> (arch::TABLE_INDEX_BITS + arch::ENTRY_OFFSET_BITS))
            & ((1 << arch::TABLE_INDEX_BITS) - 1)) as u16,
    );
    let l1_idx = PageTableIndex::new(
        ((addr_val >> arch::ENTRY_OFFSET_BITS) & ((1 << arch::TABLE_INDEX_BITS) - 1)) as u16,
    );
    (l4_idx, l3_idx, l2_idx, l1_idx)
}

/// A helper function that finds the parent page tables of a given page and unmaps them if they are empty.
///  This is used to free up memory when unmapping pages, ensuring that any empty parent page tables are also unmapped and their TLB entries are flushed.
pub fn find_free_parents_for<S: FragmentSize>(
    page: Page<S>,
    accessor: &mut impl PagetableAccessor,
) -> Result<ArrayVec<Frame<Small>, 4>, MemError> {
    let mut parents: ArrayVec<Frame<Small>, 4> = ArrayVec::new();

    let (l4_index, l3_index, l2_index, _) = dissolve_address(page.start_address());

    match S::LEVEL {
        1 => {
            // The actual page should already be unmapped, so we don't need to remove it ourselves.
            let l1_page = {
                let l1_table = accessor.l1_table(l4_index, l3_index, l2_index)?;
                if l1_table.is_empty() {
                    return Ok(parents);
                }

                l1_table.as_page()
            };

            let l2_page = {
                let l2_table = accessor.l2_table_mut(l4_index, l3_index)?;
                let l1_entry = l2_table.read_entry(l2_index);
                parents.push(Frame::from_start_address(l1_entry.addr()).unwrap());
                unsafe { l2_table.set_entry(l2_index, PageTableEntry::empty()) };
                unsafe { Flush::flush_page(l1_page) }.flush();

                if l2_table.is_empty() {
                    return Ok(parents);
                }
                l2_table.as_page()
            };

            let l3 = accessor.l3_table_mut(l4_index)?;
            let l2_entry = l3.read_entry(l3_index);
            parents.push(Frame::from_start_address(l2_entry.addr()).unwrap());
            unsafe { l3.set_entry(l3_index, PageTableEntry::empty()) };
            unsafe { Flush::flush_page(l2_page) }.flush();

            // PML4 entries are what control various access flags for the entire address space, so we don't want to remove them.
            return Ok(parents);
        }
        2 => {
            let l2_virt = {
                let l2_table = accessor.l2_table(l4_index, l3_index)?;
                if l2_table.is_empty() {
                    return Ok(parents);
                }

                l2_table.as_page()
            };

            let l3_table = accessor.l3_table_mut(l4_index)?;
            let l2_entry = l3_table.read_entry(l3_index);
            parents.push(Frame::from_start_address(l2_entry.addr()).unwrap());
            unsafe { l3_table.set_entry(l3_index, PageTableEntry::empty()) };
            unsafe { Flush::flush_page(l2_virt) }.flush();

            // PML4 entries are what control various access flags for the entire address space, so we don't want to remove them.
            return Ok(parents);
        }
        _ => return Ok(parents), // For level 3 pages, we don't have any parent tables to free. This function can also not be called for levels higher than 3.
    }
}

/// Cleans up the given range of level 4 page table entries, unmapping all memory and freeing it if appropriate.
pub unsafe fn cleanup_l4_range(
    l4_range: PageIndexIter,
    accessor: &mut impl PagetableAccessor,
    dealloc: &mut impl FullManager<FrameClass>,
    should_traverse_globals: bool,
) -> Result<(), MemError> {
    for p4_idx in l4_range {
        let l4_table = accessor.l4_table_mut()?;
        let entry = l4_table.read_entry(p4_idx);
        if !entry.is_present() || entry.flags().contains(MapFlags::GLOBAL) {
            continue;
        }

        match unsafe { cleanup_l3(p4_idx, accessor, dealloc, should_traverse_globals) } {
            Ok(()) => {}
            mem_err!(Err MalformedPageStructure::GlobalPageEncountered) => {
                if !should_traverse_globals {
                    continue;
                }
            }
            Err(e) => return Err(e),
        }

        let l4_table = accessor.l4_table_mut()?;
        unsafe { l4_table.set_entry(p4_idx, PageTableEntry::empty()) };
    }

    Ok(())
}

/// Cleans up the given level 3 page table, unmapping all memory and freeing it if appropriate.
pub unsafe fn cleanup_l3(
    l4_idx: PageTableIndex,
    accessor: &mut impl PagetableAccessor,
    dealloc: &mut impl FullManager<FrameClass>,
    should_traverse_globals: bool,
) -> Result<(), MemError> {
    let ctx = create_ctx(accessor, dealloc);
    access_and_clear_table(
        ctx,
        |c| c.accessor.l3_table_mut(l4_idx).unwrap(),
        |c, paddr| {
            let frame = Frame::<Large>::from_start_address(paddr).unwrap();
            c.dealloc.deallocate_fragment(frame);
            Ok(())
        },
        |c, idx| unsafe { cleanup_l2(l4_idx, idx, c.accessor, c.dealloc, should_traverse_globals) },
        should_traverse_globals,
        true,
    )?;

    let l4_table = accessor.l4_table_mut()?;
    let l3_entry = l4_table.read_entry(l4_idx);
    if l3_entry.flags().contains(MapFlags::DEALLOCATE) {
        let frame = Frame::<Small>::from_start_address(l3_entry.addr()).unwrap();
        dealloc.deallocate_fragment(frame);
    }

    Ok(())
}

/// Cleans up the given level 2 page table, unmapping all memory and freeing it if appropriate.
pub unsafe fn cleanup_l2(
    l4_idx: PageTableIndex,
    l3_idx: PageTableIndex,
    accessor: &mut impl PagetableAccessor,
    dealloc: &mut impl FullManager<FrameClass>,
    should_traverse_globals: bool,
) -> Result<(), MemError> {
    let ctx = create_ctx(accessor, dealloc);
    access_and_clear_table(
        ctx,
        |c| c.accessor.l2_table_mut(l4_idx, l3_idx).unwrap(),
        |c, paddr| {
            let frame = Frame::<Medium>::from_start_address(paddr).unwrap();
            c.dealloc.deallocate_fragment(frame);
            Ok(())
        },
        |c, idx| unsafe {
            cleanup_l1(
                l4_idx,
                l3_idx,
                idx,
                c.accessor,
                c.dealloc,
                should_traverse_globals,
            )
        },
        should_traverse_globals,
        true,
    )?;

    let l3_table = accessor.l3_table_mut(l4_idx)?;
    let l2_entry = l3_table.read_entry(l3_idx);
    if l2_entry.flags().contains(MapFlags::DEALLOCATE) {
        let frame = Frame::<Small>::from_start_address(l2_entry.addr()).unwrap();
        dealloc.deallocate_fragment(frame);
    }

    Ok(())
}

/// Cleans up the given level 1 page table, unmapping all memory and freeing it if appropriate.
pub unsafe fn cleanup_l1(
    l4_idx: PageTableIndex,
    l3_idx: PageTableIndex,
    l2_idx: PageTableIndex,
    accessor: &mut impl PagetableAccessor,
    dealloc: &mut impl FullManager<FrameClass>,
    should_traverse_globals: bool,
) -> Result<(), MemError> {
    let ctx = create_ctx(accessor, dealloc);
    // TODO: This approach makes it so that `c`'s type is.. `&mut &mut &mut impl PTA`..
    // I don't know if this will have any pref issues so benchmark in the future.
    access_and_clear_table(
        ctx,
        |c| c.accessor.l1_table_mut(l4_idx, l3_idx, l2_idx).unwrap(),
        |ctx, paddr| {
            let frame = Frame::<Small>::from_start_address(paddr).unwrap();
            ctx.dealloc.deallocate_fragment(frame);
            Ok(())
        },
        |_, _| Ok(()),
        false,
        should_traverse_globals,
    )?;

    let l3_table = accessor.l3_table_mut(l4_idx)?;
    let l3_entry = l3_table.read_entry(l3_idx);
    if l3_entry.flags().contains(MapFlags::DEALLOCATE) {
        let frame = Frame::<Small>::from_start_address(l3_entry.addr()).unwrap();
        dealloc.deallocate_fragment(frame);
    }

    Ok(())
}

fn access_and_clear_table<C>(
    mut ctx: C,
    mut read_table: impl FnMut(&mut C) -> &mut PageTable,
    mut free: impl FnMut(&mut C, PhysAddr) -> Result<(), MemError>,
    mut traverse: impl FnMut(&mut C, PageTableIndex) -> Result<(), MemError>,
    should_traverse_globals: bool,
    free_needs_huge: bool,
) -> Result<(), MemError> {
    let mut err = Ok(());
    for idx in PageTableIndex::iter_all() {
        let table = read_table(&mut ctx);
        let entry = table.read_entry(idx);
        if !entry.is_present() {
            continue;
        }

        if entry.flags().contains(MapFlags::GLOBAL) && !should_traverse_globals {
            err = mem_err!(Err MalformedPageStructure::GlobalPageEncountered);
            continue;
        }

        match traverse(&mut ctx, idx) {
            Ok(_) => {}
            mem_err!(Err MalformedPageStructure::GlobalPageEncountered) => {
                err = mem_err!(Err MalformedPageStructure::GlobalPageEncountered);
            }
            Err(e) => {
                return Err(e);
            }
        }

        if !entry.flags().contains(MapFlags::DEALLOCATE) {
            continue;
        }

        if free_needs_huge && !entry.is_huge() {
            continue;
        }

        free(&mut ctx, entry.addr())?;
    }

    err
}

/// An error that can occur during the copy mappings operation, such as a failure to copy mappings between page tables or address spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CopyMappingError {
    /// The source and destination ranges have mismatched sizes, which prevents the copy mappings operation from proceeding.
    #[error("The source and destination ranges have mismatched sizes.")]
    MismatchedRangeSizes,
}

pub(crate) fn copy_mappings<S, D>(
    source_table: &S,
    dest_table: &mut D,
    source_range: MemoryRange<VirtAddr>,
    dest_range: Option<MemoryRange<VirtAddr>>,
) -> Result<(), MemError>
where
    S: PagetableAccessor,
    D: PagetableAccessor + MemoryMapper,
{
    if let Some(dest_range) = dest_range {
        if source_range.size() != dest_range.size() {
            return Err(CopyMappingError::MismatchedRangeSizes.into());
        }
    }

    let source_range = source_range.align_barriers(Small::ALIGNMENT);
    let dest_range = dest_range
        .map(|vr| vr.align_barriers(Small::ALIGNMENT))
        .unwrap_or(source_range);

    for (mapping, flags) in source_table.present_mappings(source_range) {
        fn map<S: FragmentSize, D: SizedMemoryMapper<S>>(
            dst_table: &mut D,
            src_page: Page<S>,
            src_frame: Frame<S>,
            flags: MapFlags,
            dest_range: MemoryRange<VirtAddr>,
            source_range: MemoryRange<VirtAddr>,
        ) -> Result<(), MemError> {
            let src_offset = src_page.start_address().as_u64() - source_range.start().as_u64();
            let dst_page = Page::<S>::from_start_address(VirtAddr::new(
                dest_range.start().as_u64() + src_offset,
            ))
            .unwrap();
            trace_disabled!(
                "accessor.copy_mappings",
                "mapping {:?} to {:?}",
                src_page,
                dst_page
            );
            dst_table
                .map_primitive(
                    dst_page,
                    src_frame,
                    flags,
                    Some(MapFlags::DEALLOCATE),
                    &mut GlobalMemoryProvider,
                )
                .map(|flush| flush.flush())
        }

        match mapping {
            DirectMapping::Small(dst_page, src) => {
                map(dest_table, dst_page, src, flags, dest_range, source_range)?;
            }
            DirectMapping::Medium(dst_page, src) => {
                map(dest_table, dst_page, src, flags, dest_range, source_range)?;
            }
            DirectMapping::Large(dst_page, src) => {
                map(dest_table, dst_page, src, flags, dest_range, source_range)?;
            }
        }
    }

    Ok(())
}

struct CleanupCtx<'a, T: PagetableAccessor, M: FullManager<FrameClass>> {
    accessor: &'a mut T,
    dealloc: &'a mut M,
}

fn create_ctx<'a, T: PagetableAccessor, M: FullManager<FrameClass>>(
    accessor: &'a mut T,
    dealloc: &'a mut M,
) -> CleanupCtx<'a, T, M> {
    CleanupCtx { accessor, dealloc }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_and_dissolve_address() {
        let l4_idx = PageTableIndex::new(511);
        let l3_idx = PageTableIndex::new(2);
        let l2_idx = PageTableIndex::new(3);
        let l1_idx = PageTableIndex::new(4);

        let addr = ((build_address(l4_idx, l3_idx, l2_idx, l1_idx) as i64) << 16 >> 16) as u64; //Sign extend
        let (dissolved_l4, dissolved_l3, dissolved_l2, dissolved_l1) =
            dissolve_address(VirtAddr::new(addr));

        assert_eq!(l4_idx, dissolved_l4);
        assert_eq!(l3_idx, dissolved_l3);
        assert_eq!(l2_idx, dissolved_l2);
        assert_eq!(l1_idx, dissolved_l1);

        let addr = build_vaddress(l4_idx, l3_idx, l2_idx, l1_idx);
        let (dissolved_l4, dissolved_l3, dissolved_l2, dissolved_l1) = dissolve_address(addr);
        assert_eq!(l4_idx, dissolved_l4);
        assert_eq!(l3_idx, dissolved_l3);
        assert_eq!(l2_idx, dissolved_l2);
        assert_eq!(l1_idx, dissolved_l1);
    }
}
