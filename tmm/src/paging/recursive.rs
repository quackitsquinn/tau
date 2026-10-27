use core::{fmt::Debug, iter::Map};

use arrayvec::ArrayVec;
use cake::log::error;

use crate::{
    MapFlags, MemError,
    err::MalformedPageStructure,
    mem_err,
    paging::{
        Address, FragmentManager, FragmentSize, Frame, Large, Medium, MemoryFragment, MemoryRange,
        Page, PageTable, PageTableEntry, PageTableIndex, Small, VirtAddr,
        accessor::{self, PagetableAccessor, TablePointer},
        map::{Flush, MemoryMapper, SizedMemoryMapper, Unmapped},
        primitives::{AnyPage, DirectMapping, VirtRange},
        table,
        translate::{Translate, TranslateResult},
    },
};

/// A recursive page table that allows for mapping and unmapping of pages in a virtual address space.
pub struct RecursivePageTable<'a> {
    pub(crate) mut(super) root_table: &'a mut PageTable,
    pub(crate) mut(super) recursive_index: PageTableIndex,
}

impl Debug for RecursivePageTable<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RecursivePageTable")
            .field("recursive_index", &self.recursive_index)
            .finish()
    }
}

impl<'a> RecursivePageTable<'a> {
    pub(crate) unsafe fn new(
        root_table: &'a mut PageTable,
        recursive_index: PageTableIndex,
    ) -> Self {
        Self {
            root_table,
            recursive_index,
        }
    }

    pub(crate) fn p4(&self) -> &PageTable {
        self.root_table
    }

    pub(crate) fn p4_mut(&mut self) -> &mut PageTable {
        self.root_table
    }

    pub(crate) fn recursive_index(&self) -> PageTableIndex {
        self.recursive_index
    }

    /// Returns the table at the given table pointer, if it exists.
    pub unsafe fn table(&self, ptr: TablePointer) -> Result<&PageTable, MemError> {
        let (l4, l3, l2) = ptr.dissolve();
        match ptr.level() {
            4 => Ok(self.p4()),
            3 => self.l3_table(l4),
            2 => self.l2_table(l4, l3),
            1 => self.l1_table(l4, l3, l2),
            _ => unreachable!(),
        }
    }
    /// Returns the table at the given table pointer, if it exists.
    pub unsafe fn table_mut(&mut self, ptr: TablePointer) -> Result<&mut PageTable, MemError> {
        let (l4, l3, l2) = ptr.dissolve();
        match ptr.level() {
            4 => Ok(self.p4_mut()),
            3 => self.l3_table_mut(l4),
            2 => self.l2_table_mut(l4, l3),
            1 => self.l1_table_mut(l4, l3, l2),
            _ => unreachable!(),
        }
    }

    fn default_parent_flags() -> MapFlags {
        // There's some weirdness with the way the CPU handles
        // parent table flags. If the parent isn't writable, all children are read-only,
        // However, instead of having an EXECUTABLE bit, the CPU (at least on x86_64)
        // has a "no-execute" or NX bit. The issue is that if the parent is marked as NX,
        // all children are NX. This caused a GIANT headache and genuinely hours of debugging.
        MapFlags::WRITABLE | MapFlags::EXECUTABLE
    }

    fn l3_table_or_alloc(
        &mut self,
        l4_index: PageTableIndex,
        table_flags: MapFlags,
        allocator: &mut impl FragmentManager<Frame<Small>, Small>,
    ) -> Result<&mut PageTable, MemError> {
        let l4_entry = self.p4_mut().read_entry(l4_index);
        if !l4_entry.is_present() {
            let new_frame = allocator.allocate_fragment()?;
            unsafe {
                self.p4_mut()
                    .set_entry(l4_index, PageTableEntry::new(new_frame, table_flags));
            }
            let table = self.l3_table_mut(l4_index)?;
            unsafe { table.zero() };
            return Ok(table);
        }
        self.l3_table_mut(l4_index)
    }

    fn l2_table_or_alloc(
        &mut self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
        table_flags: MapFlags,
        allocator: &mut impl FragmentManager<Frame<Small>, Small>,
    ) -> Result<&mut PageTable, MemError> {
        let l3_table = self.l3_table_or_alloc(l4_index, table_flags, allocator)?;
        let l3_entry = l3_table.read_entry(l3_index);
        if !l3_entry.is_present() {
            let new_frame = allocator.allocate_fragment()?;
            unsafe {
                l3_table.set_entry(l3_index, PageTableEntry::new(new_frame, table_flags));
            }
            let table = self.l2_table_mut(l4_index, l3_index)?;
            unsafe { table.zero() };
            return Ok(table);
        }
        self.l2_table_mut(l4_index, l3_index)
    }

    fn l1_table_or_alloc(
        &mut self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
        l2_index: PageTableIndex,
        table_flags: MapFlags,
        allocator: &mut impl FragmentManager<Frame<Small>, Small>,
    ) -> Result<&mut PageTable, MemError> {
        let l2_table = self.l2_table_or_alloc(l4_index, l3_index, table_flags, allocator)?;
        let l2_entry = l2_table.read_entry(l2_index);
        if !l2_entry.is_present() {
            let new_frame = allocator.allocate_fragment()?;
            unsafe {
                l2_table.set_entry(l2_index, PageTableEntry::new(new_frame, table_flags));
            }
            let table = self.l1_table_mut(l4_index, l3_index, l2_index)?;
            unsafe { table.zero() };
            return Ok(table);
        }
        self.l1_table_mut(l4_index, l3_index, l2_index)
    }
}

impl SizedMemoryMapper<Large> for RecursivePageTable<'_> {
    fn map_primitive<A>(
        &mut self,
        dst: super::Page<Large>,
        src: super::Frame<Large>,
        flags: crate::MapFlags,
        parent_table_flags: Option<crate::MapFlags>,
        allocator: &mut A,
    ) -> Result<super::map::Flush, crate::MemError>
    where
        A: super::FragmentManager<super::Frame<super::Small>, super::Small>,
    {
        let (l4, l3, _, _) = accessor::dissolve_address(dst.start_address());

        let table_flags = parent_table_flags.unwrap_or_default() | Self::default_parent_flags();
        let l3_table = self.l3_table_or_alloc(l4, table_flags, allocator)?;
        unsafe {
            l3_table.set_entry(l3, PageTableEntry::new(src, flags).huge());
        }
        Ok(unsafe { Flush::flush_page(dst) })
    }

    unsafe fn unmap_primitive(
        &mut self,
        page: super::Page<Large>,
    ) -> Result<Unmapped<Large>, crate::MemError> {
        let (l4, l3, _, _) = accessor::dissolve_address(page.start_address());
        let table = self.l3_table_mut(l4)?;
        let entry = table.read_entry(l3);
        if !entry.is_present() {
            return mem_err!(Err MalformedPageStructure::NotMapped(AnyPage::Large(page)));
        } else if !entry.is_huge() {
            return mem_err!(Err MalformedPageStructure::MappedToLowerLevel(page.start_address()));
        }
        unsafe {
            table.set_entry(l3, PageTableEntry::empty());
        }

        Ok(Unmapped::new(
            Frame::from_start_address(entry.addr()).unwrap(),
            accessor::find_free_parents_for(page, &mut *self)?,
            unsafe { Some(Flush::flush_page(page)) },
            entry.flags(),
        ))
    }
}

impl SizedMemoryMapper<Medium> for RecursivePageTable<'_> {
    fn map_primitive<A>(
        &mut self,
        dst: super::Page<Medium>,
        src: super::Frame<Medium>,
        flags: crate::MapFlags,
        parent_table_flags: Option<crate::MapFlags>,
        allocator: &mut A,
    ) -> Result<super::map::Flush, crate::MemError>
    where
        A: super::FragmentManager<super::Frame<super::Small>, super::Small>,
    {
        let (l4, l3, l2, _) = accessor::dissolve_address(dst.start_address());

        let table_flags = parent_table_flags.unwrap_or_default() | Self::default_parent_flags();
        let l2_table = self.l2_table_or_alloc(l4, l3, table_flags, allocator)?;
        unsafe {
            l2_table.set_entry(l2, PageTableEntry::new(src, flags).huge());
        }
        Ok(unsafe { Flush::flush_page(dst) })
    }

    unsafe fn unmap_primitive(
        &mut self,
        page: super::Page<Medium>,
    ) -> Result<super::map::Unmapped<Medium>, crate::MemError> {
        let (l4, l3, l2, _) = accessor::dissolve_address(page.start_address());
        let table = self.l2_table_mut(l4, l3)?;
        let entry = table.read_entry(l2);
        if !entry.is_present() {
            return mem_err!(Err MalformedPageStructure::NotMapped(AnyPage::Medium(page)));
        } else if !entry.is_huge() {
            return mem_err!(Err MalformedPageStructure::MappedToLowerLevel(page.start_address()));
        }
        unsafe {
            table.set_entry(l2, PageTableEntry::empty());
        }

        Ok(Unmapped::new(
            Frame::from_start_address(entry.addr()).unwrap(),
            accessor::find_free_parents_for(page, &mut *self)?,
            unsafe { Some(Flush::flush_page(page)) },
            entry.flags(),
        ))
    }
}

impl SizedMemoryMapper<Small> for RecursivePageTable<'_> {
    fn map_primitive<A>(
        &mut self,
        dst: super::Page<Small>,
        src: super::Frame<Small>,
        flags: crate::MapFlags,
        parent_table_flags: Option<crate::MapFlags>,
        allocator: &mut A,
    ) -> Result<super::map::Flush, crate::MemError>
    where
        A: super::FragmentManager<super::Frame<super::Small>, super::Small>,
    {
        let (l4, l3, l2, l1) = accessor::dissolve_address(dst.start_address());

        let table_flags = parent_table_flags.unwrap_or_default() | Self::default_parent_flags();
        let l1_table = self.l1_table_or_alloc(l4, l3, l2, table_flags, allocator)?;
        unsafe {
            l1_table.set_entry(l1, PageTableEntry::new(src, flags));
        }
        Ok(unsafe { Flush::flush_page(dst) })
    }

    unsafe fn unmap_primitive(
        &mut self,
        page: super::Page<Small>,
    ) -> Result<super::map::Unmapped<Small>, crate::MemError> {
        let (l4, l3, l2, l1) = accessor::dissolve_address(page.start_address());
        let table = self.l1_table_mut(l4, l3, l2)?;
        let entry = table.read_entry(l1);
        if !entry.is_present() {
            return mem_err!(Err MalformedPageStructure::NotMapped(AnyPage::Small(page)));
        }

        unsafe {
            table.set_entry(l1, PageTableEntry::empty());
        }

        Ok(Unmapped::new(
            Frame::from_start_address(entry.addr()).unwrap(),
            accessor::find_free_parents_for(page, &mut *self)?,
            unsafe { Some(Flush::flush_page(page)) },
            entry.flags(),
        ))
    }
}

impl PagetableAccessor for RecursivePageTable<'_> {
    fn read_l4_table(&self) -> Result<crate::paging::VirtAddr, MemError> {
        Ok(self.p4().as_virt())
    }

    fn read_l3_table(&self, l4_index: PageTableIndex) -> Result<crate::paging::VirtAddr, MemError> {
        let addr_raw = accessor::build_address(
            self.recursive_index,
            self.recursive_index,
            self.recursive_index,
            l4_index,
        );

        if !self.p4().read_entry(l4_index).is_present() {
            return mem_err!(Err MalformedPageStructure::PagetableNotPresent);
        }

        Ok(VirtAddr::new_truncate(addr_raw))
    }

    fn read_l2_table(
        &self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
    ) -> Result<crate::paging::VirtAddr, MemError> {
        let addr_raw = accessor::build_address(
            self.recursive_index,
            self.recursive_index,
            l4_index,
            l3_index,
        );

        let entry = self.l3_table(l4_index)?.read_entry(l3_index);

        if !entry.is_present() {
            return mem_err!(Err MalformedPageStructure::PagetableNotPresent);
        }

        if entry.is_huge() {
            return mem_err!(Err MalformedPageStructure::MappedToHigherLevel(
                VirtAddr::new_truncate(accessor::build_address(
                    l4_index,
                    l3_index,
                    PageTableIndex::new(0),
                    PageTableIndex::new(0),
                )),
            ));
        }

        Ok(VirtAddr::new_truncate(addr_raw))
    }

    fn read_l1_table(
        &self,
        l4_index: PageTableIndex,
        l3_index: PageTableIndex,
        l2_index: PageTableIndex,
    ) -> Result<crate::paging::VirtAddr, MemError> {
        let addr_raw = accessor::build_address(self.recursive_index, l4_index, l3_index, l2_index);

        let entry = self.l2_table(l4_index, l3_index)?.read_entry(l2_index);
        if !entry.is_present() {
            return mem_err!(Err MalformedPageStructure::PagetableNotPresent);
        }

        if entry.is_huge() {
            return mem_err!(Err MalformedPageStructure::MappedToHigherLevel(
                VirtAddr::new_truncate(accessor::build_address(
                    l4_index,
                    l3_index,
                    l2_index,
                    PageTableIndex::new(0)
                ),)
            ));
        }

        Ok(VirtAddr::new_truncate(addr_raw))
    }
}

impl MemoryMapper for RecursivePageTable<'_> {}
