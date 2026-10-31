use core::{
    alloc::Layout,
    fmt::Debug,
    mem::{Alignment, MaybeUninit},
};

use cake::{
    limine::memory_map::EntryType,
    log::{error, info},
    trace_disabled,
};

use crate::{
    MapFlags, MemError, align,
    bitmap::{
        Bitmap, BitmapBacking, VirtualMemoryManager,
        managers::{
            address_as_bit_index, align_in_bits, alignment_of, bit_index_as_address,
            entries_for_bytes, n_pages_for_bytes,
        },
    },
    entry_walker::EntryWalker,
    paging::{
        Address, AddressExt, FragmentManager, FragmentSize, Frame, FullManager, Large, Medium,
        MemoryFragment, PhysAddr, Small,
        map::DataAllocator,
        map_from,
        primitives::{FrameClass, MemoryRange},
    },
};

/// A physical memory manager that uses bitmaps to track the allocation of physical frames.
#[derive(Debug)]
pub struct PhysicalMemoryManager {
    bitmaps: &'static mut [BitmapEntry],
}

impl PhysicalMemoryManager {
    /// Initializes the physical memory manager with the given memory map and virtual memory manager.
    pub unsafe fn init(
        mut entry_walker: EntryWalker,
        vmm: &mut VirtualMemoryManager,
    ) -> Result<Self, MemError> {
        let entries = entry_walker.entries;
        let n_avail = entries
            .iter()
            .filter(|e| e.entry_type == EntryType::USABLE)
            .count();

        let slice_layout = Layout::array::<BitmapEntry>(n_avail).unwrap();
        let vmem = vmm
            .allocate(slice_layout)
            .expect("Failed to allocate memory for bitmap entries");

        unsafe {
            map_from(
                vmem,
                MapFlags::WRITABLE,
                None,
                &mut DataAllocator(&mut entry_walker),
                &mut (),
            )?
        };

        let bitmaps = unsafe {
            core::slice::from_raw_parts_mut(
                vmem.start().as_mut_ptr::<MaybeUninit<BitmapEntry>>(),
                n_avail,
            )
        };

        let mut bitmap_iter = entries.iter().filter(|e| e.entry_type == EntryType::USABLE);
        for i in 0..n_avail {
            let entry = bitmap_iter.next().unwrap();
            bitmaps[i] = MaybeUninit::new(Self::allocate_bitmap(
                MemoryRange::new_len(PhysAddr::new(entry.base), entry.length),
                &mut entry_walker,
                vmm,
            )?);
        }

        let bitmaps = unsafe { core::mem::transmute::<_, &mut [BitmapEntry]>(bitmaps) };
        bitmaps.sort_unstable_by_key(|e| e.free);

        for bitmap in bitmaps.iter() {
            // info!(
            //     "Initialized bitmap for range: start={:#x}, size={} bytes, alignment={:x?}, free frames={}",
            //     bitmap.start.as_u64(),
            //     bitmap.size(),
            //     bitmap.byte_alignment(),
            //     bitmap.free
            // );
            info!("Init bitmap: {:?}", bitmap);
        }

        let mut pmm = Self { bitmaps };

        for used in entry_walker.used_regions() {
            info!(
                "Marking used region as allocated: start={:#x}, size={} bytes",
                used.base, used.length
            );

            unsafe { pmm.mark_allocated(used.into()) };
        }

        info!("entry_walker final state: {:#?}", entry_walker);

        Ok(pmm)
    }

    fn allocate_bitmap(
        range: MemoryRange<PhysAddr>,
        walker: &mut EntryWalker,
        vmm: &mut VirtualMemoryManager,
    ) -> Result<BitmapEntry, crate::MemError> {
        let placed = EntryPlacement::place(range);
        info!("placing bitmap: {:?}", placed);
        let allocated_range = vmm
            .allocate(placed.buf_layout)
            .ok_or(MemError::OutOfMemory)?;
        unsafe {
            map_from(
                allocated_range,
                MapFlags::WRITABLE,
                None,
                &mut DataAllocator(walker),
                &mut (),
            )?
        };

        let bitmap_slice = unsafe {
            core::slice::from_raw_parts_mut(
                allocated_range.start().as_mut_ptr::<u64>(),
                placed.n_entries,
            )
        };

        for entry in bitmap_slice.iter_mut() {
            *entry = 0;
        }

        Ok(BitmapEntry {
            start: placed.base,
            bitmap: Bitmap::init(
                BitmapBacking::ManuallyManaged(bitmap_slice),
                placed.n_remainder,
            ),
            bit_alignment: placed.max_alignment,
            free: range.size() / Small::SIZE,
        })
    }

    fn bitmaps_for<S: FragmentSize>(&mut self) -> impl Iterator<Item = &mut BitmapEntry> {
        let bit_align = align_in_bits(Alignment::new(S::SIZE as usize).unwrap());
        let n_bits = n_pages_for_bytes(S::SIZE);
        self.bitmaps
            .iter_mut()
            // filter out bitmaps that cannot satisfy the alignment or size requirements
            // size isn't really a thing we can concretely check for without complicated bit run logic (which might be worth doing later),
            // but we can at least filter out bitmaps that don't have enough free bits to satisfy the request
            .filter(move |b| b.bit_alignment >= bit_align && b.free >= n_bits)
    }

    // Returns a mutable reference to the bitmap entry that contains the given physical frame, if any.
    // This is O(n) given the number of bitmap entries.
    fn bitmap_containing<S: FragmentSize>(
        &mut self,
        primitive: Frame<S>,
    ) -> Option<&mut BitmapEntry> {
        let addr = primitive.start_address();
        self.bitmaps
            .iter_mut()
            .find(|b| addr >= b.start && addr < b.start + b.size())
    }

    /// Returns a mutable reference to the bitmap entry that contains the given physical address range, if any.
    /// This is O(n) given the number of bitmap entries.
    fn bitmap_for_range(&mut self, range: MemoryRange<PhysAddr>) -> Option<&mut BitmapEntry> {
        self.bitmaps
            .iter_mut()
            .find(|b| range.start() >= b.start && range.end() <= b.end())
    }

    unsafe fn mark_allocated(&mut self, range: MemoryRange<PhysAddr>) {
        if let Some(bmp) = self.bitmap_for_range(range) {
            info!("using bitmap for range: {:?} {:?}", bmp, range);
            let bitptr = address_as_bit_index(range.start(), bmp.start).expect(
                "address must be within the managed physical address space and properly aligned",
            );
            let n_bits = n_pages_for_bytes(range.size());
            bmp.bitmap.set(bitptr, n_bits);
            info!(
                "updating free, was {}, subtracting {}, now {:?}",
                bmp.free,
                n_bits,
                bmp.free.checked_sub(n_bits)
            );
            bmp.free -= n_bits;
        } else {
            panic!("address range is not within any managed physical memory range");
        }
    }

    unsafe fn mark_unallocated(&mut self, range: MemoryRange<PhysAddr>) {
        if let Some(bmp) = self.bitmap_for_range(range) {
            let bitptr = address_as_bit_index(range.start(), bmp.start).expect(
                "address must be within the managed physical address space and properly aligned",
            );
            let n_bits = n_pages_for_bytes(range.size());
            bmp.bitmap.clear(bitptr, n_bits);
            bmp.free += n_bits;
        } else {
            panic!("address range is not within any managed physical memory range");
        }
    }
}

unsafe impl<S> FragmentManager<Frame<S>, S> for PhysicalMemoryManager
where
    S: FragmentSize,
{
    fn allocate_fragment(&mut self) -> Result<Frame<S>, MemError> {
        for bitmap in self.bitmaps_for::<S>() {
            trace_disabled!(
                "tmm.bitmap.phys.alloc_fragment",
                "looking in bitmap with len {} and align {:x}",
                bitmap.free,
                bitmap.bit_alignment.as_usize()
            );
            if let Some(bitptr) = bitmap.bitmap.allocate(S::BITS, align_in_bits(S::ALIGNMENT)) {
                trace_disabled!("tmm.bitmap.phys.alloc_fragment", "found free: {:?}", bitptr);
                bitmap.free -= S::BITS;
                let addr = bit_index_as_address(bitptr.bit_index(), bitmap.start);
                return Ok(Frame::from_start_address(addr).unwrap());
            }
        }

        trace_disabled!("tmm.bitmap.phys.alloc_fragment", "OOM!");
        Err(MemError::OutOfMemory)
    }

    fn deallocate_fragment(&mut self, primitive: Frame<S>) {
        if let Some(bitmap) = self.bitmap_containing(primitive) {
            let bitptr = address_as_bit_index(primitive.start_address(), bitmap.start)
                .expect("deallocated address must be within the managed physical address space and properly aligned");
            debug_assert!(bitmap.bitmap.all_are_set(bitptr, S::BITS));
            bitmap.bitmap.clear(bitptr, S::BITS);
            bitmap.free += S::BITS;
        } else {
            panic!("deallocated address is not within any managed physical memory range");
        }
    }
}

impl FullManager<FrameClass> for PhysicalMemoryManager {}

struct BitmapEntry {
    // the bitmap that tracks the allocation of frames in this range
    bitmap: Bitmap<'static>,
    // the start of this entry
    start: PhysAddr,
    /// the max alignment that this bitmap can guarantee for it's allocations.
    ///
    /// there will be a way to configure how many entries in the manager that are aligned to higher alignments,
    /// which removes any alignment logic from the bitmap itself. ideally there will be ranges that do map well to higher alignments, but
    /// we can just align up to the next alignment boundary, which yes, does waste some memory, but it is simpler and removes the weird alignment logic that a unaligned
    /// bitmap would require.
    bit_alignment: Alignment,
    /// the total number of free frames in this bitmap. used to skip bitmaps that are full.
    free: u64,
}

impl Debug for BitmapEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BitmapEntry")
            .field("start", &format_args!("{:#x}", self.start.as_u64()))
            .field("size", &self.size())
            .field("alignment", &self.byte_alignment())
            .field("free", &self.free)
            .finish()
    }
}

impl BitmapEntry {
    fn size(&self) -> u64 {
        self.bitmap.n_bits() * Small::SIZE
    }

    fn byte_alignment(&self) -> Alignment {
        Alignment::new(self.bit_alignment.as_usize() * Small::SIZE as usize).unwrap()
    }

    fn end(&self) -> PhysAddr {
        self.start + self.size()
    }
}

#[derive(Debug)]
struct EntryPlacement {
    base: PhysAddr,
    buf_layout: Layout,
    n_entries: usize,
    n_remainder: u8,
    max_alignment: Alignment,
}

const N_HIGHER_FOR_BUMP: u64 = 12;

impl EntryPlacement {
    fn place(range: MemoryRange<PhysAddr>) -> Self {
        let base_align = range.start_align();
        let size = range.size();

        let (new_base, new_align) = if size > Large::SIZE * N_HIGHER_FOR_BUMP {
            (
                align!(up, range.start().as_u64(), Large::SIZE),
                Large::ALIGNMENT,
            )
        } else if size > Medium::SIZE * N_HIGHER_FOR_BUMP {
            (
                align!(up, range.start().as_u64(), Medium::SIZE),
                Medium::ALIGNMENT,
            )
        } else {
            (
                align!(up, range.start().as_u64(), Small::SIZE),
                Small::ALIGNMENT,
            )
        };

        let skipped_bytes = new_base - range.start().as_u64();
        let size = size - skipped_bytes;
        let n_pages = n_pages_for_bytes(size);
        let n_entries = entries_for_bytes(size);
        let bit_align = align_in_bits(new_align);

        Self {
            base: PhysAddr::new(new_base),
            buf_layout: Layout::array::<u64>(n_entries as usize).unwrap(),
            n_entries: n_entries as usize,
            n_remainder: (n_pages % 64) as u8,
            max_alignment: bit_align,
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
}
