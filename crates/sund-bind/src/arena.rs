//! Bump arena allocator.
//!
//! Ported from `ndec/impl/bind.c` §1.

const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;
const ALIGN: usize = 16; // max_align_t equivalent on 64-bit

/// Chunk header, followed by inline payload bytes.
struct Chunk {
    next: Option<Box<Chunk>>,
    cap: usize,
    used: usize,
    data: Vec<u8>,
}

impl Chunk {
    fn new(cap: usize) -> Self {
        Self {
            next: None,
            cap,
            used: 0,
            data: vec![0u8; cap],
        }
    }
}

/// Bump arena allocator.
///
/// All allocations come from contiguous chunks; the entire arena is freed
/// in one shot via `drop` (or explicitly via `destroy`).
pub struct Arena {
    head: Option<Box<Chunk>>,
    chunk_size_hint: usize,
}

fn align_up(n: usize, a: usize) -> usize {
    (n + (a - 1)) & !(a - 1)
}

impl Arena {
    /// Create a new empty arena. Does not allocate.
    pub fn new() -> Self {
        Self {
            head: None,
            chunk_size_hint: DEFAULT_CHUNK_SIZE,
        }
    }

    /// Release every chunk owned by the arena. Idempotent.
    pub fn destroy(&mut self) {
        self.head = None;
    }

    /// Keep chunks, reset bump cursors to beginning. Safe to reuse.
    pub fn reset(&mut self) {
        let mut c = &mut self.head;
        while let Some(chunk) = c {
            chunk.used = 0;
            c = &mut chunk.next;
        }
    }

    /// Allocate `n` bytes. Returns a mutable pointer to the allocation,
    /// or `None` on OOM (only if the system allocator fails).
    pub fn alloc(&mut self, n: usize) -> Option<*mut u8> {
        if n == 0 {
            // Return a non-null sentinel for zero-size allocs.
            return Some(std::ptr::NonNull::dangling().as_ptr());
        }

        // Try the current head chunk.
        if let Some(ref mut chunk) = self.head {
            let off = align_up(chunk.used, ALIGN);
            if off + n <= chunk.cap {
                chunk.used = off + n;
                return Some(chunk.data.as_mut_ptr().wrapping_add(off));
            }
        }

        // Need a new chunk.
        let new_cap = self.chunk_size_hint.max(n);
        let mut nc = Box::new(Chunk::new(new_cap));
        nc.used = n;
        let ptr = nc.data.as_mut_ptr();
        nc.next = self.head.take();
        self.head = Some(nc);
        Some(ptr)
    }

    /// Allocate `len + 1` bytes, copy `src[0..len)`, NUL-terminate.
    /// Returns `None` on OOM.
    pub fn memdup_z(&mut self, src: &[u8]) -> Option<*mut u8> {
        let len = src.len();
        let p = self.alloc(len + 1)?;
        unsafe {
            if len > 0 {
                std::ptr::copy_nonoverlapping(src.as_ptr(), p, len);
            }
            *p.add(len) = 0;
        }
        Some(p)
    }

    /// Allocate and copy bytes. Returns a mutable pointer.
    pub fn alloc_copy(&mut self, src: &[u8]) -> Option<*mut u8> {
        let p = self.alloc(src.len())?;
        if !src.is_empty() {
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), p, src.len());
            }
        }
        Some(p)
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        self.destroy();
    }
}
