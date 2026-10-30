use std::{
    alloc::{self, Layout},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy)]
pub struct Info {
    pub layout: Layout,
    pub drop: unsafe fn(*mut u8, usize),
    pub sparse: bool,
    pub summary: bool,
}
impl Info {
    pub fn of<T>(sparse: bool, summary: bool) -> Self {
        unsafe fn drop_values<T>(pointer: *mut u8, len: usize) {
            unsafe { ptr::drop_in_place(ptr::slice_from_raw_parts_mut(pointer.cast::<T>(), len)) };
        }
        Self {
            layout: Layout::new::<T>(),
            drop: drop_values::<T>,
            sparse,
            summary,
        }
    }
    fn allocation(self, capacity: usize) -> Layout {
        Layout::from_size_align(
            self.layout
                .size()
                .checked_mul(capacity)
                .expect("column capacity overflow"),
            self.layout.align(),
        )
        .expect("column capacity overflow")
    }
}
pub(crate) struct Summary {
    pub added: AtomicU64,
    pub changed: AtomicU64,
}

/// Raw buffers; the owning store supplies their common length and capacity.
pub(crate) struct Column {
    pub info: Info,
    pub data: *mut u8,
    pub added: *mut u64,
    pub changed: *mut u64,
    pub summary: Option<Box<Summary>>,
}
// These private buffers are only owned by World<S>, whose schema retains T's
// Send/Sync requirements. No independently transferable public owner exists.
unsafe impl Send for Column {}
unsafe impl Sync for Column {}
impl Column {
    pub fn new(info: Info) -> Self {
        Self {
            info,
            data: ptr::without_provenance_mut(info.layout.align()),
            added: ptr::NonNull::<u64>::dangling().as_ptr(),
            changed: ptr::NonNull::<u64>::dangling().as_ptr(),
            summary: info.summary.then(|| {
                Box::new(Summary {
                    added: AtomicU64::new(0),
                    changed: AtomicU64::new(0),
                })
            }),
        }
    }
    pub fn validate_capacity(&self, capacity: usize) {
        self.info.allocation(capacity);
        Layout::array::<u64>(capacity).expect("tick capacity overflow");
    }
    pub unsafe fn grow(&mut self, old: usize, new: usize) {
        unsafe fn resize(pointer: *mut u8, old: Layout, new: Layout) -> *mut u8 {
            if new.size() == 0 {
                return pointer;
            }
            let result = if old.size() == 0 {
                unsafe { alloc::alloc(new) }
            } else {
                unsafe { alloc::realloc(pointer, old, new.size()) }
            };
            if result.is_null() {
                alloc::handle_alloc_error(new);
            }
            result
        }
        // Layouts were checked for every column before any buffer moves. The
        // remaining allocator failure aborts, rather than unwinding half-grown.
        unsafe {
            self.data = resize(
                self.data,
                self.info.allocation(old),
                self.info.allocation(new),
            );
            let old = Layout::array::<u64>(old).unwrap();
            let new = Layout::array::<u64>(new).unwrap();
            self.added = resize(self.added.cast(), old, new).cast();
            self.changed = resize(self.changed.cast(), old, new).cast();
        }
    }
    pub unsafe fn transfer(&self, row: usize, target: &Self, target_row: usize) {
        unsafe {
            ptr::copy_nonoverlapping(
                self.data.add(row * self.info.layout.size()),
                target.data.add(target_row * self.info.layout.size()),
                self.info.layout.size(),
            );
            let added = *self.added.add(row);
            let changed = *self.changed.add(row);
            target.added.add(target_row).write(added);
            target.changed.add(target_row).write(changed);
            if let Some(summary) = &target.summary {
                summary.added.fetch_max(added, Ordering::Relaxed);
                summary.changed.fetch_max(changed, Ordering::Relaxed);
            }
        }
    }
    pub unsafe fn remove_uninit(&self, row: usize, last: usize) {
        if row != last {
            unsafe {
                ptr::copy_nonoverlapping(
                    self.data.add(last * self.info.layout.size()),
                    self.data.add(row * self.info.layout.size()),
                    self.info.layout.size(),
                );
                self.added.add(row).write(*self.added.add(last));
                self.changed.add(row).write(*self.changed.add(last));
            }
        }
    }
    pub unsafe fn swap_to_last(&self, row: usize, last: usize) {
        if row != last {
            unsafe {
                ptr::swap_nonoverlapping(
                    self.data.add(row * self.info.layout.size()),
                    self.data.add(last * self.info.layout.size()),
                    self.info.layout.size(),
                );
                ptr::swap(self.added.add(row), self.added.add(last));
                ptr::swap(self.changed.add(row), self.changed.add(last));
            }
        }
    }
    unsafe fn release(&mut self, capacity: usize) {
        unsafe {
            let layout = self.info.allocation(capacity);
            if layout.size() != 0 {
                alloc::dealloc(self.data, layout);
            }
            if capacity != 0 {
                let layout = Layout::array::<u64>(capacity).unwrap();
                alloc::dealloc(self.added.cast(), layout);
                alloc::dealloc(self.changed.cast(), layout);
            }
        }
    }
    unsafe fn dispose(&mut self, len: usize, capacity: usize) {
        struct Release<'a>(&'a mut Column, usize);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                unsafe { self.0.release(self.1) }
            }
        }
        let guard = Release(self, capacity);
        unsafe { (guard.0.info.drop)(guard.0.data, len) };
    }
}

pub(crate) struct Store {
    pub entities: Vec<crate::Entity>,
    pub indices: Vec<usize>,
    pub columns: Vec<Column>,
    pub capacity: usize,
    pub archetypes: Vec<usize>,
}
impl Store {
    pub fn new(indices: Vec<usize>, info: &[Info]) -> Self {
        let columns = indices.iter().map(|&i| Column::new(info[i])).collect();
        Self {
            entities: Vec::new(),
            indices,
            columns,
            capacity: 0,
            archetypes: Vec::new(),
        }
    }
    pub fn reserve(&mut self, additional: usize) -> bool {
        if additional <= self.capacity - self.entities.len() {
            return false;
        }
        let required = self
            .entities
            .len()
            .checked_add(additional)
            .expect("table capacity overflow");
        let capacity = required.max(self.capacity.saturating_mul(2)).max(4);
        for column in &self.columns {
            column.validate_capacity(capacity);
        }
        self.entities.reserve_exact(capacity - self.entities.len());
        for column in &mut self.columns {
            unsafe { column.grow(self.capacity, capacity) };
        }
        self.capacity = capacity;
        true
    }
    /// Move retained values; removed values must already have been taken.
    pub unsafe fn transfer(&self, row: usize, target: &Self) {
        let target_row = target.entities.len();
        let mut j = 0;
        for (i, &component) in self.indices.iter().enumerate() {
            while j < target.indices.len() && target.indices[j] < component {
                j += 1;
            }
            if target.indices.get(j) == Some(&component) {
                unsafe { self.columns[i].transfer(row, &target.columns[j], target_row) };
            }
        }
        let last = self.entities.len() - 1;
        for column in &self.columns {
            unsafe { column.remove_uninit(row, last) };
        }
    }
    /// Move the removed values out of the live prefix without dropping them.
    pub unsafe fn prepare_drop(&self, row: usize) {
        let last = self.entities.len() - 1;
        for column in &self.columns {
            unsafe { column.swap_to_last(row, last) };
        }
    }
    pub unsafe fn drop_row(&self, row: usize) {
        struct Finish<'a>(std::slice::Iter<'a, Column>, usize);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                for column in self.0.by_ref() {
                    unsafe {
                        (column.info.drop)(column.data.add(self.1 * column.info.layout.size()), 1)
                    };
                }
            }
        }
        let mut finish = Finish(self.columns.iter(), row);
        for column in finish.0.by_ref() {
            unsafe { (column.info.drop)(column.data.add(row * column.info.layout.size()), 1) };
        }
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        struct Finish<'a>(std::slice::IterMut<'a, Column>, usize, usize);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                for column in self.0.by_ref() {
                    unsafe { column.dispose(self.1, self.2) };
                }
            }
        }
        let mut finish = Finish(self.columns.iter_mut(), self.entities.len(), self.capacity);
        for column in finish.0.by_ref() {
            unsafe { column.dispose(finish.1, finish.2) };
        }
    }
}
