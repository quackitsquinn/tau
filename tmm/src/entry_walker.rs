//! A helper struct for iterating over memory map entries and calculating the total usable memory.
use core::fmt::Debug;
use core::{mem, ops, slice};

use arrayvec::ArrayVec;
use cake::limine::memory_map::{Entry, EntryType};
use cake::log::error;

use crate::paging::limine::LimineEntry;
use crate::paging::primitives::FrameClass;
use crate::paging::{
    Address, FragmentManager, FragmentSize, Frame, FullManager, MemoryFragment, MemoryRange,
    PhysAddr,
};
use crate::{MemError, align};

const MAX_FRAGMENTS: usize = 0x40;

/// A helper struct for iterating over memory map entries and calculating the total usable memory.
#[derive(Debug)]
pub struct EntryWalker<'a> {
    /// The underlying entries provided from Limine
    pub entries: EntrySource<'a>,
    current: MemoryRegion,
    current_idx: usize,
    fragments: ArrayVec<MemoryRegion, MAX_FRAGMENTS>,
}

/// A struct representing a region of memory with a base address and length.
///
/// This is in a different format than MemoryRegion due to how Limine returns memory map entries.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct MemoryRegion {
    /// The base address of the memory region.
    pub base: u64,
    /// The length of the memory region in bytes.
    pub length: u64,
}

impl MemoryRegion {
    #[inline]
    fn align_for_with_offset<S: FragmentSize>(&self) -> Option<(PhysAddr, u64)> {
        let aligned_base = align!(up, self.base, S::SIZE);
        let offset = aligned_base - self.base;
        if self.length >= S::SIZE + offset {
            Some((PhysAddr::new(aligned_base), offset))
        } else {
            None
        }
    }
}

impl From<MemoryRegion> for MemoryRange<PhysAddr> {
    fn from(region: MemoryRegion) -> Self {
        MemoryRange::new_len(PhysAddr::new(region.base), region.length)
    }
}

impl From<LimineEntry> for MemoryRegion {
    fn from(entry: LimineEntry) -> Self {
        // SAFETY: ExtraEntry has the same memory layout as the first 2 fields of Entry, so we can transmute it without any issues.
        // We only need the base and length fields for the extra entries, since the entry type is not relevant for them.
        unsafe { mem::transmute_copy(&entry) }
    }
}

impl Debug for MemoryRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExtraEntry")
            .field("base", &format_args!("{:#x}", self.base))
            .field("length", &format_args!("{:#x}", self.length))
            .finish()
    }
}

/// A wrapper around a slice of Limine memory map entries that can either be borrowed or owned.
#[derive(Clone)]
pub enum EntrySource<'a> {
    /// A borrowed slice of Limine memory map entries.
    Borrowed(&'a [LimineEntry]),
    /// An owned vector of Limine memory map entries.
    Owned(ArrayVec<LimineEntry, 32>),
}

impl<'a> Debug for EntrySource<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EntrySource::Borrowed(entries) => f
                .debug_tuple("EntrySource::Borrowed")
                .field(&format_args!("{:?}", entries))
                .finish(),
            EntrySource::Owned(entries) => f
                .debug_tuple("EntrySource::Owned")
                .field(&format_args!("{:?}", entries))
                .finish(),
        }
    }
}

impl<'a> From<&'a [LimineEntry]> for EntrySource<'a> {
    fn from(entries: &'a [LimineEntry]) -> Self {
        EntrySource::Borrowed(entries)
    }
}

impl<'a> ops::Deref for EntrySource<'a> {
    type Target = [LimineEntry];

    fn deref(&self) -> &Self::Target {
        match self {
            EntrySource::Borrowed(entries) => entries,
            EntrySource::Owned(entries) => entries.as_slice(),
        }
    }
}

impl<'a> EntryWalker<'a> {
    /// Creates a new `EntryWalker` with the given slice of memory map entries.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the provided memory map entries are valid and that the layout described by the entries remains valid for the lifetime of the `EntryWalker`.
    pub unsafe fn new(memory_map: EntrySource<'a>) -> Result<Self, MemError> {
        let entries = unsafe {
            slice::from_raw_parts(memory_map.as_ptr() as *const LimineEntry, memory_map.len())
        };

        let (current_idx, current) = entries
            .iter()
            .enumerate()
            .find(|e| e.1.entry_type == EntryType::USABLE)
            .ok_or(MemError::OutOfMemory)?;

        Ok(Self {
            entries: memory_map,
            current: MemoryRegion::from(*current),
            current_idx,
            fragments: ArrayVec::new(),
        })
    }

    /// Creates a new `EntryWalker` with the given slice of Limine memory map entries.
    pub(crate) unsafe fn from_limine_entries(entries: &'a [LimineEntry]) -> Result<Self, MemError> {
        unsafe { Self::new(EntrySource::Borrowed(entries)) }
    }

    /// Returns the amount of usable memory in bytes.
    pub fn usable_memory(&self) -> u64 {
        let mut total = 0;
        for entry in &*self.entries {
            if matches!(
                entry.entry_type,
                EntryType::USABLE | EntryType::BOOTLOADER_RECLAIMABLE | EntryType::ACPI_RECLAIMABLE
            ) {
                total += entry.length;
            }
        }
        total
    }

    fn current_region(&self) -> Result<MemoryRegion, MemError> {
        if self.current_idx >= self.entries.len() {
            return Err(MemError::OutOfMemory);
        }

        Ok(self.current)
    }

    fn advance_region(&mut self) -> Result<MemoryRegion, MemError> {
        if self.current.length != 0 {
            self.fragments.push(self.current);
            self.fragments.sort_unstable_by_key(|r| r.length);
        }
        self.current_idx += 1;
        while self.current_idx < self.entries.len() {
            let entry = self.entries[self.current_idx];
            if entry.entry_type == EntryType::USABLE {
                self.current = MemoryRegion::from(entry);
                return Ok(self.current);
            }
            self.current_idx += 1;
        }

        Err(MemError::OutOfMemory)
    }

    fn fragment_for<S: FragmentSize>(&mut self) -> Option<Frame<S>> {
        if self.fragments.is_empty() {
            return None;
        }

        if self.fragments.last().unwrap().length < S::SIZE {
            return None;
        }

        for i in 0..self.fragments.len() {
            let fragment = self.fragments[i];
            let (aligned_base, offset) = match fragment.align_for_with_offset::<S>() {
                Some(aligned) => aligned,
                None => continue,
            };
            let new_base = aligned_base + S::SIZE;
            let new_length = fragment.length - S::SIZE - offset;
            if new_length == 0 {
                self.fragments.remove(i);
            } else {
                self.fragments[i] = MemoryRegion {
                    base: new_base.as_u64(),
                    length: new_length,
                };
            }
            return Some(Frame::from_start_address(aligned_base).unwrap());
        }
        None
    }

    /// Returns an iterator over the used regions of memory.
    pub fn used_regions(&self) -> UsedRegions<'_> {
        UsedRegions::new(self)
    }

    /// Allocates a frame of the given size from the available memory regions.
    pub fn allocate_for<S: FragmentSize>(&mut self) -> Result<Frame<S>, MemError> {
        self.allocate_fragment()
    }
}

/// An iterator over the used regions of memory.
#[derive(Debug)]
pub struct UsedRegions<'a> {
    walker: &'a EntryWalker<'a>,
    partial: Option<MemoryRegion>,
    index: usize,
}

impl<'a> UsedRegions<'a> {
    /// Creates a new `UsedRegions` iterator for the given `EntryWalker`.
    pub fn new(walker: &'a EntryWalker<'a>) -> Self {
        let partial = match walker.current_region() {
            Ok(region) => {
                let full = walker.entries[walker.current_idx];
                Some(MemoryRegion {
                    base: full.base,
                    length: region.base - full.base,
                })
            }
            Err(_) => None,
        };
        Self {
            walker,
            index: 0,
            partial,
        }
    }
}

impl Iterator for UsedRegions<'_> {
    type Item = MemoryRegion;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(partial) = self.partial.take() {
            return Some(partial);
        }

        while self.index < self.walker.current_idx {
            let entry = self.walker.entries[self.index];
            self.index += 1;
            if entry.entry_type == EntryType::USABLE {
                return Some(MemoryRegion::from(entry));
            }
        }

        None
    }
}

unsafe impl<S> FragmentManager<Frame<S>, S> for EntryWalker<'_>
where
    S: FragmentSize,
{
    fn allocate_fragment(&mut self) -> Result<Frame<S>, MemError> {
        // Check if we have any fragments that can fit the requested size
        if let Some(frame) = self.fragment_for::<S>() {
            return Ok(frame);
        }

        let region = self.current_region()?;
        let (aligned, offset) = match region.align_for_with_offset::<S>() {
            Some(aligned) => aligned,
            None => {
                self.advance_region()?;
                return self.allocate_fragment();
            }
        };

        let new_base = aligned + S::SIZE;
        let new_length = region.length - S::SIZE - offset;
        if new_length == 0 {
            // It's important that we tell the walker that this region is now fully used, so we can advance to the next one.
            self.current.length = 0;
            // We don't care if the walker runs out of usable regions, since it doesn't affect us.
            let _ = self.advance_region();
        } else {
            self.current = MemoryRegion {
                base: new_base.as_u64(),
                length: new_length,
            };
        }

        Ok(Frame::from_start_address(aligned).unwrap())
    }

    fn deallocate_fragment(&mut self, _primitive: Frame<S>) {
        // We don't need to do anything here since the EntryWalker is only used for initial bootstrapping and we won't be deallocating any frames during that process, but we need to implement this method to satisfy the PrimitiveRangeManager trait.
        error!("EntryWalker<{}>::deallocate_range called", S::NAME);
    }
}

impl FullManager<FrameClass> for EntryWalker<'_> {}

#[cfg(test)]
mod tests {
    use cake::limine::memory_map::EntryType;
    use x86_64::structures::paging::frame;

    use crate::{
        MemError,
        arch::{L1_PAGE_SIZE, L2_PAGE_SIZE, L3_PAGE_SIZE},
        entry_walker::{EntryWalker, MemoryRegion},
        paging::{
            Address, FragmentManager, Frame, FullManager, Large, Medium, MemoryFragment, PhysAddr,
            Small, limine::LimineEntry,
        },
    };

    #[test]
    fn test_region_alignment() {
        use super::*;

        fn test<S: FragmentSize>(base: u64, length: u64, expected: Option<(u64, u64)>) {
            let region = MemoryRegion { base, length };
            let result = region.align_for_with_offset::<S>();
            match (result, expected) {
                (Some((aligned, offset)), Some((expected_aligned, expected_offset))) => {
                    assert_eq!(aligned.as_u64(), expected_aligned);
                    assert_eq!(offset, expected_offset);
                }
                (None, None) => {}
                _ => panic!(
                    "Expected {:?} but got {:?} for base: {:#x}, length: {:#x}",
                    expected, result, base, length
                ),
            }
        }

        test::<Small>(L1_PAGE_SIZE, L1_PAGE_SIZE, Some((L1_PAGE_SIZE, 0)));
        test::<Small>(
            L1_PAGE_SIZE + 1,
            L1_PAGE_SIZE * 2,
            Some((L1_PAGE_SIZE * 2, L1_PAGE_SIZE - 1)),
        );
        test::<Small>(L1_PAGE_SIZE, L1_PAGE_SIZE - 1, None);
        test::<Medium>(L2_PAGE_SIZE, L2_PAGE_SIZE, Some((L2_PAGE_SIZE, 0)));
        test::<Medium>(
            L2_PAGE_SIZE + 1,
            L2_PAGE_SIZE * 2,
            Some((L2_PAGE_SIZE * 2, L2_PAGE_SIZE - 1)),
        );
        test::<Medium>(L2_PAGE_SIZE + 1, L2_PAGE_SIZE, None);
        test::<Large>(L3_PAGE_SIZE, L3_PAGE_SIZE, Some((L3_PAGE_SIZE, 0)));
        test::<Large>(
            L3_PAGE_SIZE + 1,
            L3_PAGE_SIZE * 2,
            Some((L3_PAGE_SIZE * 2, L3_PAGE_SIZE - 1)),
        );
        test::<Large>(L3_PAGE_SIZE + 1, L3_PAGE_SIZE, None);
    }

    fn create_entries(entries: &[(u64, u64, EntryType)]) -> Vec<LimineEntry> {
        entries
            .iter()
            .map(|&(base, length, entry_type)| LimineEntry {
                base,
                length,
                entry_type,
            })
            .collect()
    }

    #[test]
    fn test_walker_usable_memory() {
        let entries = create_entries(&[
            (0x1000, 0x1000, EntryType::USABLE),
            (0x2000, 0x1000, EntryType::RESERVED),
            (0x3000, 0x1000, EntryType::USABLE),
            (0x4000, 0x1000, EntryType::ACPI_RECLAIMABLE),
            (0x5000, 0x1000, EntryType::BOOTLOADER_RECLAIMABLE),
            (0x6000, 0x1000, EntryType::KERNEL_AND_MODULES),
        ]);
        let refs: Vec<&LimineEntry> = entries.iter().collect();
        let walker = unsafe { EntryWalker::from_limine_entries(&refs).unwrap() };
        assert_eq!(walker.usable_memory(), 0x4000);
    }

    #[test]
    fn test_walker_allocate_basic_oom() {
        let entries = create_entries(&[
            (0x1000, 0x1000, EntryType::USABLE),
            (0x2000, 0x1000, EntryType::RESERVED),
            (0x3000, 0x1000, EntryType::USABLE),
        ]);
        let refs: Vec<&LimineEntry> = entries.iter().collect();
        let mut walker = unsafe { EntryWalker::from_limine_entries(&refs).unwrap() };

        let frame1: Frame<Small> = walker.allocate_for().unwrap();
        assert_eq!(frame1.start_address().as_u64(), 0x1000);

        let frame2: Frame<Small> = walker.allocate_for().unwrap();
        assert_eq!(frame2.start_address().as_u64(), 0x3000);

        assert_eq!(walker.allocate_for::<Small>(), Err(MemError::OutOfMemory));
    }

    #[test]
    fn test_walker_allocate_higher_alignment() {
        let entries = create_entries(&[
            (0x1000, 0x3000, EntryType::USABLE),
            (L2_PAGE_SIZE, L2_PAGE_SIZE, EntryType::USABLE),
            (L2_PAGE_SIZE * 2 + 0x2000, 0x1000, EntryType::USABLE),
        ]);
        let refs: Vec<&LimineEntry> = entries.iter().collect();
        let mut walker = unsafe { EntryWalker::from_limine_entries(&refs).unwrap() };

        let frame1: Frame<Small> = walker.allocate_for().unwrap();
        assert_eq!(frame1.start_address().as_u64(), 0x1000);

        let frame2: Frame<Medium> = walker.allocate_for().unwrap();
        assert_eq!(frame2.start_address().as_u64(), L2_PAGE_SIZE);

        let frame3: Frame<Small> = walker.allocate_for().unwrap();
        assert_eq!(frame3.start_address().as_u64(), 0x2000);

        let frame4: Frame<Small> = walker.allocate_for().unwrap();
        assert_eq!(frame4.start_address().as_u64(), 0x3000);

        let frame5: Frame<Small> = walker.allocate_for().unwrap();
        assert_eq!(frame5.start_address().as_u64(), L2_PAGE_SIZE * 2 + 0x2000);

        assert_eq!(walker.allocate_for::<Medium>(), Err(MemError::OutOfMemory));
    }

    #[test]
    fn test_walker_used_regions() {
        let entries = create_entries(&[
            (0x1000, 0x1000, EntryType::USABLE),
            (0x2000, 0x1000, EntryType::RESERVED),
            (0x3000, 0x2000, EntryType::USABLE),
            (0x5000, 0x1000, EntryType::ACPI_RECLAIMABLE),
        ]);
        let refs: Vec<&LimineEntry> = entries.iter().collect();
        let mut walker = unsafe { EntryWalker::from_limine_entries(&refs).unwrap() };
        assert_eq!(
            walker.allocate_for::<Small>(),
            Ok(Frame::from_start_address(PhysAddr::new(0x1000)).unwrap())
        );
        assert_eq!(
            walker.allocate_for::<Small>(),
            Ok(Frame::from_start_address(PhysAddr::new(0x3000)).unwrap())
        );

        let used_regions: Vec<MemoryRegion> = walker.used_regions().collect();
        println!("Used regions: {:?}", used_regions);
        assert_eq!(used_regions.len(), 2);
        assert!(used_regions.contains(&MemoryRegion {
            base: 0x1000,
            length: 0x1000
        }));
        assert!(used_regions.contains(&MemoryRegion {
            base: 0x3000,
            length: 0x1000
        }));
    }
}
