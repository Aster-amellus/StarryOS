//! File readahead implementation for StarryOS
//!
//! This module implements a Linux-like readahead mechanism with adaptive window sizing.
//! The algorithm detects sequential access patterns and prefetches pages ahead of the
//! current read position to improve I/O performance.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use axfs_ng::FileBackend;

/// Page size in bytes (4KB)
pub const PAGE_SIZE: u64 = 4096;

/// Initial readahead size in pages
const RA_INIT_PAGES: u32 = 4;

/// Maximum readahead size in pages (256KB = 64 pages)
const RA_MAX_PAGES: u32 = 64;

/// Minimum readahead size in pages (reserved for future use)
#[allow(dead_code)]
const RA_MIN_PAGES: u32 = 2;

/// Maximum allowed gap between reads to still be considered sequential (in pages)
const RA_SEQ_GAP_PAGES: u64 = 2;

/// Readahead access pattern
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RaPattern {
    /// Initial state, no pattern detected yet
    Initial = 0,
    /// Sequential access detected
    Sequential = 1,
    /// Random access detected
    Random = 2,
}

impl From<u32> for RaPattern {
    fn from(value: u32) -> Self {
        match value {
            1 => Self::Sequential,
            2 => Self::Random,
            _ => Self::Initial,
        }
    }
}

/// Readahead state for a file (similar to Linux's `file_ra_state`)
///
/// This structure tracks the readahead window and access patterns for a file.
/// It uses atomic operations to allow concurrent access without locks.
pub struct ReadaheadState {
    ra_start: AtomicU64,
    ra_size: AtomicU32,
    /// Async trigger point: when reads reach (ra_start + ra_size - async_size),
    /// trigger the next async readahead (in pages)
    async_size: AtomicU32,
    prev_end: AtomicU64,
    pattern: AtomicU32,
    /// Number of consecutive sequential reads
    seq_count: AtomicU32,
}

impl Default for ReadaheadState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadaheadState {
    /// Create a new readahead state
    pub const fn new() -> Self {
        Self {
            ra_start: AtomicU64::new(0),
            ra_size: AtomicU32::new(0),
            async_size: AtomicU32::new(0),
            prev_end: AtomicU64::new(0),
            pattern: AtomicU32::new(RaPattern::Initial as u32),
            seq_count: AtomicU32::new(0),
        }
    }

    /// Get the current access pattern
    #[inline]
    pub fn pattern(&self) -> RaPattern {
        self.pattern.load(Ordering::Relaxed).into()
    }

    /// Check if the current read should trigger async readahead
    fn should_trigger_async(&self, read_start: u64) -> bool {
        let ra_start = self.ra_start.load(Ordering::Relaxed);
        let ra_size = self.ra_size.load(Ordering::Relaxed) as u64 * PAGE_SIZE;
        let async_size = self.async_size.load(Ordering::Relaxed) as u64 * PAGE_SIZE;

        if ra_size == 0 {
            return false;
        }

        // Trigger when read position enters the async window
        let trigger_point = ra_start + ra_size.saturating_sub(async_size);
        read_start >= trigger_point && read_start < ra_start + ra_size
    }

    /// Update readahead window
    fn update_window(&self, start: u64, size_pages: u32, async_pages: u32) {
        self.ra_start.store(start, Ordering::Relaxed);
        self.ra_size.store(size_pages, Ordering::Relaxed);
        self.async_size.store(async_pages, Ordering::Relaxed);
    }

    /// Calculate next readahead size with exponential growth
    fn next_ra_size(&self) -> u32 {
        let current = self.ra_size.load(Ordering::Relaxed);
        if current == 0 {
            RA_INIT_PAGES
        } else {
            // Double the size, but cap at maximum
            (current * 2).min(RA_MAX_PAGES)
        }
    }

    /// Detect access pattern and update state
    ///
    /// Returns (is_sequential, is_cache_hit) tuple
    fn detect_pattern(&self, read_start: u64, read_len: usize, cache_hit: bool) -> (bool, bool) {
        let prev_end = self.prev_end.swap(read_start + read_len as u64, Ordering::Relaxed);
        let pattern = self.pattern();

        // Check if this is a sequential read
        let gap = if read_start >= prev_end {
            read_start - prev_end
        } else {
            prev_end - read_start
        };

        let is_sequential = if prev_end == 0 {
            // First read - assume sequential if starting from beginning
            read_start < PAGE_SIZE * 4
        } else {
            // Allow small gaps for sequential detection
            gap <= RA_SEQ_GAP_PAGES * PAGE_SIZE
        };

        if is_sequential {
            let count = self.seq_count.fetch_add(1, Ordering::Relaxed);
            if pattern != RaPattern::Sequential && count >= 2 {
                self.pattern
                    .store(RaPattern::Sequential as u32, Ordering::Relaxed);
            }
        } else {
            self.seq_count.store(0, Ordering::Relaxed);
            self.pattern
                .store(RaPattern::Random as u32, Ordering::Relaxed);
            // Reset readahead window on random access
            self.ra_size.store(0, Ordering::Relaxed);
        }

        (is_sequential, cache_hit)
    }
}

/// Readahead decision result
pub enum ReadaheadAction {
    /// No readahead needed
    None,
    /// Perform synchronous readahead (for initial reads or cache misses)
    Sync { start_page: u32, num_pages: u32 },
    /// Perform asynchronous readahead (trigger next window)
    Async { start_page: u32, num_pages: u32 },
}

/// Make a readahead decision based on current access
///
/// This function should be called before each read operation.
/// It returns the recommended readahead action.
pub fn readahead_decide(
    state: &ReadaheadState,
    backend: &FileBackend,
    read_start: u64,
    read_len: usize,
) -> ReadaheadAction {
    if read_len == 0 {
        return ReadaheadAction::None;
    }

    let start_page = (read_start / PAGE_SIZE) as u32;
    let cache_hit = backend.is_page_cached(start_page);

    // Detect access pattern
    let (is_sequential, _) = state.detect_pattern(read_start, read_len, cache_hit);

    if !is_sequential {
        return ReadaheadAction::None;
    }

    // Check if we should trigger async readahead
    if state.should_trigger_async(read_start) {
        let ra_start = state.ra_start.load(Ordering::Relaxed);
        let ra_size = state.ra_size.load(Ordering::Relaxed);

        // Next window starts at current window end
        let next_start = ra_start + ra_size as u64 * PAGE_SIZE;
        let next_size = state.next_ra_size();
        let async_size = next_size / 4; // 25% of window for async trigger

        // Update window for next iteration
        state.update_window(next_start, next_size, async_size);

        return ReadaheadAction::Async {
            start_page: (next_start / PAGE_SIZE) as u32,
            num_pages: next_size,
        };
    }

    // Initial readahead on cache miss with sequential pattern
    if !cache_hit && state.pattern() != RaPattern::Random {
        let ra_size = RA_INIT_PAGES;
        let async_size = ra_size / 4;

        // Set initial window
        let window_start = (start_page as u64) * PAGE_SIZE;
        state.update_window(window_start, ra_size, async_size.max(1));

        return ReadaheadAction::Sync {
            start_page,
            num_pages: ra_size,
        };
    }

    ReadaheadAction::None
}

/// Execute synchronous readahead
///
/// This function prefetches pages synchronously into the page cache.
pub fn do_sync_readahead(backend: &FileBackend, start_page: u32, num_pages: u32) -> usize {
    backend.prefetch_pages(start_page, num_pages)
}

/// Execute asynchronous readahead
///
/// This function spawns a background task to prefetch pages.
/// Note: The actual spawning should be done by the caller to avoid
/// tight coupling with the task system.
#[inline]
pub fn should_async_readahead(action: &ReadaheadAction) -> Option<(u32, u32)> {
    match action {
        ReadaheadAction::Async {
            start_page,
            num_pages,
        } => Some((*start_page, *num_pages)),
        _ => None,
    }
}

/// Calculate page number from byte offset
#[inline]
pub const fn offset_to_page(offset: u64) -> u32 {
    (offset / PAGE_SIZE) as u32
}

/// Calculate byte offset from page number
#[inline]
pub const fn page_to_offset(page: u32) -> u64 {
    page as u64 * PAGE_SIZE
}
