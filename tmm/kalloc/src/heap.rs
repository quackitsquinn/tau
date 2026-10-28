use core::{alloc::Layout, cmp::Ordering, mem, ptr::NonNull, slice};

use alloc::{alloc::AllocError, vec::Vec};
use tmm::{
    ContextualizedError, MapFlags, MapSource, MemError, align,
    paging::{AddressExt, heapless::HeaplessAllocator, primitives::VirtRange},
};

use crate::HeapConfig;

/// A simple heap allocator that manages a contiguous range of virtual memory for dynamic allocation.
#[derive(Debug)]
pub struct Heap {
    /// The configuration for the heap, including initial and maximum sizes.
    pub mut(self) config: HeapConfig,
    metadata: Vec<Option<Free>, HeaplessAllocator>,
    first_free: usize,
    by_size: Vec<u32, HeaplessAllocator>, // < now u32
    by_addr: Vec<u32, HeaplessAllocator>, // < now u32
    heap_range: VirtRange,
}

impl Heap {
    /// Creates a new heap allocator with the specified configuration.
    pub fn new(config: HeapConfig) -> Result<Self, ContextualizedError> {
        let vrange = tmm::reserve_virtual(
            Layout::from_size_align(config.max_heap_size, config.max_heap_size).unwrap(),
        )?;

        tmm::map(
            vrange.truncate(config.initial_heap_size as u64),
            MapSource::Anon,
            MapFlags::WRITABLE,
            None,
            &mut (),
        )?;

        let ha = HeaplessAllocator::new(MapFlags::WRITABLE);

        let mut new = Self {
            config,
            metadata: Vec::new_in(ha),
            first_free: 0,
            by_size: Vec::new_in(ha),
            by_addr: Vec::new_in(ha),
            heap_range: vrange,
        };
        new.push_free(Free::new(
            vrange.start().as_mut_ptr(),
            new.config.initial_heap_size,
        ));
        Ok(new)
    }

    fn push_raw(&mut self, free: Free) -> usize {
        let mut slot = None;
        for i in self.first_free..self.metadata.len() {
            if self.metadata[i].is_none() {
                self.first_free = i;
                slot = Some(i);
                break;
            }
        }

        let idx = slot.unwrap_or_else(|| {
            self.metadata.push(None);
            self.metadata.len() - 1
        });

        if idx > u32::MAX as usize {
            panic!("Heap metadata index overflow");
        }

        self.metadata[idx] = Some(free);

        idx
    }

    fn push_free(&mut self, free: Free) -> usize {
        let idx = self.push_raw(free);

        self.insert_addr(idx);
        self.insert_size(idx);

        idx
    }

    fn read_meta(&self, idx: u32) -> &Free {
        self.metadata[idx as usize]
            .as_ref()
            .expect("index pointed at vacant free!")
    }

    fn write_meta(&mut self, idx: u32, free: Free) {
        self.metadata[idx as usize] = Some(free);
    }

    fn clear_meta(&mut self, idx: u32) {
        self.metadata[idx as usize] = None;
    }

    fn find_free_for_layout(&self, layout: Layout) -> Option<usize> {
        let size = layout.size();
        let align = layout.align();

        let size_idx = find_index(
            &self.metadata,
            &self.by_size,
            size + align.saturating_sub(1),
            |_, free| {
                let free = free.as_ref().expect("index pointed at vacant free!");
                free.size()
            },
        );

        if size_idx >= self.by_size.len() {
            return None;
        }

        for i in size_idx..self.by_size.len() {
            let free = self.read_meta(self.by_size[i]);
            let aligned = align!(up, free.start as usize, layout.align());
            let padding = aligned - free.start as usize;

            if padding + layout.size() <= free.size() {
                return Some(i);
            }
        }

        None
    }

    /// Allocates a block of memory with the specified layout from the heap.
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let free_idx = self.find_free_for_layout(layout).ok_or(AllocError)?;
        let free = self.metadata[self.by_size[free_idx] as usize]
            .as_mut()
            .expect("index pointed at vacant free!");
        debug_assert!(free.size() >= layout.size());

        let aligned_base = align!(up, free.start as usize, layout.align()) as *mut ();
        let padding = aligned_base as usize - free.start as usize;
        let total_size = padding + layout.size();
        let remaining_size = free.size() - total_size;

        let new_free = Free::new(
            (aligned_base as usize + layout.size()) as *mut u8,
            remaining_size,
        );
        self.reclaim_free(new_free);

        let slice = unsafe { slice::from_raw_parts_mut(aligned_base as *mut u8, layout.size()) };
        Ok(NonNull::new(slice).ok_or(AllocError)?)
    }

    fn insert_addr(&mut self, metadata_idx: usize) -> usize {
        let free = self.metadata[metadata_idx]
            .as_ref()
            .expect("index pointed at vacant free!");
        let index = find_index(&self.metadata, &self.by_addr, free.start, |_, free| {
            free.as_ref().expect("index pointed at vacant free!").start
        });
        self.by_addr.insert(index, metadata_idx as u32);
        index
    }

    fn insert_size(&mut self, metadata_idx: usize) -> usize {
        let free = self.metadata[metadata_idx]
            .as_ref()
            .expect("index pointed at vacant free!");
        let index = find_index(&self.metadata, &self.by_size, free.size(), |_, free| {
            free.as_ref().expect("index pointed at vacant free!").size()
        });
        self.by_size.insert(index, metadata_idx as u32);
        index
    }

    fn find_size_index_for_free(&self, free: &Free) -> Option<usize> {
        let mut size_idx = bs_try_find(
            &self.metadata,
            &self.by_size,
            free.size(),
            |_, free_slot| free_slot.as_ref().unwrap().size(),
        )?;

        loop {
            let meta_idx = self.by_size[size_idx];
            let meta_free = self.read_meta(meta_idx);
            if meta_free.start == free.start {
                return Some(size_idx);
            }

            if size_idx + 1 >= self.by_size.len()
                || self.read_meta(self.by_size[size_idx + 1]).size() != free.size()
            {
                return None;
            }

            size_idx += 1;
        }
    }

    fn try_find_addr<A>(&self, value: A, key: impl Fn(u32, &Free) -> A) -> Option<usize>
    where
        A: Ord,
    {
        bs_try_find(&self.metadata, &self.by_addr, value, |i, free| {
            key(i, free.as_ref().unwrap())
        })
    }

    fn reclaim_free(&mut self, free: Free) {
        let mut curr = free;
        let mut reclaimed_low = None;
        let mut reclaimed_high = None;
        loop {
            if let Some(other_idx) = self.try_find_addr(curr.start, |_, free| free.end) {
                let meta_idx = self.by_addr[other_idx];
                let other = self.read_meta(meta_idx as u32);
                if other.end == curr.start {
                    curr.merge(other);
                    reclaimed_low = Some(meta_idx);
                    reclaimed_high.get_or_insert(meta_idx);
                    continue;
                }
            }

            if let Some(other_idx) = self.try_find_addr(curr.end, |_, free| free.start) {
                let meta_idx = self.by_addr[other_idx];
                let other = self.read_meta(meta_idx as u32);
                if other.end == curr.start {
                    curr.merge(other);
                    reclaimed_high = Some(meta_idx);
                    reclaimed_low.get_or_insert(meta_idx);
                    continue;
                }
            }

            break;
        }

        if reclaimed_low.is_none() && reclaimed_high.is_none() {
            self.push_free(curr);
            return;
        }

        let low_idx = reclaimed_low.unwrap();
        let high_idx = reclaimed_high.unwrap_or(low_idx);

        for idx in low_idx..=high_idx {
            let meta_idx = self.by_addr[idx as usize];
            let size_idx = self
                .find_size_index_for_free(&self.read_meta(meta_idx))
                .unwrap();
            self.by_size.remove(size_idx);
            self.clear_meta(meta_idx);
        }

        let swap_entry = low_idx;
        let idx = self.push_raw(curr);
        self.by_addr.drain(low_idx as usize + 1..=high_idx as usize);
        mem::swap(&mut self.by_addr[swap_entry as usize], &mut (idx as u32));
        self.insert_size(idx);
    }

    /// Deallocates a block of memory previously allocated from the heap.
    pub unsafe fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) {
        let free = Free::new(ptr.as_ptr(), layout.size());
        self.reclaim_free(free);
    }
}

fn find_index<T, A>(data: &[T], indices: &[u32], value: A, key: impl Fn(u32, &T) -> A) -> usize
where
    A: Ord,
{
    let mut low = 0;
    let mut high = data.len();
    while low < high {
        let mid = (low + high) / 2;
        let index = indices[mid];
        let mid_value = &data[index as usize];
        if key(index, mid_value) < value {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    low
}
/// Binary search for a value in a slice of data, using a slice of indices to access the data.
///
/// Returns the index of the found value in the indices slice, or None if not found.
fn bs_try_find<T, A>(
    data: &[T],
    indices: &[u32],
    value: A,
    key: impl Fn(u32, &T) -> A,
) -> Option<usize>
where
    A: Ord,
{
    let mut low = 0;
    let mut high = data.len();
    while low < high {
        let mid = (low + high) / 2;
        let index = indices[mid];
        let mid_value = &data[index as usize];
        match key(index, mid_value).cmp(&value) {
            Ordering::Less => low = mid + 1,
            Ordering::Greater | Ordering::Equal => high = mid,
        }
    }

    if low < data.len() && key(indices[low], &data[indices[low] as usize]) == value {
        Some(low)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Free {
    start: *mut u8,
    end: *mut u8,
}

impl Free {
    fn new(start: *mut u8, size: usize) -> Self {
        Self {
            start,
            end: unsafe { start.add(size) },
        }
    }

    fn size(&self) -> usize {
        (self.end as usize).saturating_sub(self.start as usize)
    }

    fn merge(&mut self, other: &Free) {
        if self.end == other.start {
            self.end = other.end;
        } else if self.start == other.end {
            self.start = other.start;
        } else {
            panic!("Cannot merge non-adjacent free blocks");
        }
    }
}
