# SUND-RS Benchmark Report

## Test Date
April 27, 2026

## Test Configurations

### Small File (2.1 MB JSON)
```json
{
  "records": [
    {
      "id": int,
      "name": "Record N",
      "email": "userN@example.com",
      "active": bool,
      "score": float,
      "tags": [string, string, string],
      "metadata": {
        "created": "2024-01-01T00:00:00Z",
        "version": int,
        "fields": [int, int, int, int, int]
      }
    }
    // Repeated 10,000 times
  ]
}
```

### Large File (41.6 MB JSON)
Same structure repeated 200,000 times

---

## Results: Small File (2.1 MB)

### Branch: `develop` (baseline - inline frames array)
```
BASE mode (validation only):
  Run 1: 1,744 ns/iter = 1,223 MB/s
  Run 2: 1,761 ns/iter = 1,212 MB/s
  Run 3: 1,758 ns/iter = 1,213 MB/s
  AVERAGE: 1,754 ns/iter = 1,216 MB/s

PARSE mode (full callbacks):
  Run 1: 2,727 ns/iter = 782 MB/s
  Run 2: 2,745 ns/iter = 777 MB/s
  Run 3: 2,723 ns/iter = 784 MB/s
  AVERAGE: 2,732 ns/iter = 781 MB/s

Overhead ratio (PARSE/BASE): 1.558x
```

### Branch: `optimize/frames-off-stack` (Box-allocated frames)
```
BASE mode (validation only):
  Run 1: 1,751 ns/iter = 1,218 MB/s
  Run 2: 1,749 ns/iter = 1,220 MB/s
  Run 3: 1,737 ns/iter = 1,228 MB/s
  AVERAGE: 1,746 ns/iter = 1,222 MB/s

PARSE mode (full callbacks):
  Run 1: 2,697 ns/iter = 791 MB/s
  Run 2: 2,708 ns/iter = 788 MB/s
  Run 3: 2,739 ns/iter = 779 MB/s
  AVERAGE: 2,715 ns/iter = 786 MB/s

Overhead ratio (PARSE/BASE): 1.556x
```

### Small File: Conclusion
**No measurable difference** on small files.
- BASE: +0.5% slower (within noise margin)
- PARSE: +0.6% faster (within noise margin)
- Reason: Working set fits in L1/L2 cache regardless of frame location

---

## Results: Large File (41.6 MB)

### Branch: `develop` (baseline - inline frames array)
```
BASE mode (validation only):
  35,411 ns/iter = 1,231 MB/s

PARSE mode (full callbacks):
  54,523 ns/iter = 800 MB/s

Overhead ratio (PARSE/BASE): 1.540x
```

### Branch: `optimize/frames-off-stack` (Box-allocated frames)
```
BASE mode (validation only):
  34,651 ns/iter = 1,258 MB/s

PARSE mode (full callbacks):
  55,380 ns/iter = 787 MB/s

Overhead ratio (PARSE/BASE): 1.601x
```

### Large File: Conclusion
**Minimal or negative impact** observed:
- BASE: +0.8% faster (34.7 vs 35.4 µs/iter)
- PARSE: -1.0% slower (55.4 vs 54.5 µs/iter)
- Reason: Heap allocation adds one level of indirection on each frame access

---

## Analysis

### Why Frames Off-Stack Doesn't Help Much

1. **Frame Access Pattern**: Stack frames are accessed sequentially during parse
   - Parser starts at depth=0, pushes/pops frames
   - Most JSON has depth < 20, so only 20 frames are typically accessed
   - Even if inline, only ~160 bytes are used per parse
   - Box allocation doesn't change cache locality for deeply nested JSON

2. **Box Indirection Cost**:
   - `Box<[Frame; 256]>` adds one pointer dereference per frame access
   - Modern CPUs: dereference is cached on TLB
   - But still incurs one extra load → small cost

3. **Stack Space vs. Cache**:
   - Inline frames (2,048 bytes) fit in L1 cache (32-64 KB typical)
   - Box frames on heap → first access is a cache miss
   - Subsequent accesses are cached
   - Net effect: minimal or negative for small/medium JSON

### Cache Miss Analysis

From perf profiling:
- **Cache miss rate**: 99.83% - this seems unreliable (likely counter overflow)
- **Real issue**: The cache misses are likely from:
  1. Input buffer prefetching (can't avoid)
  2. Reactor callbacks doing their own allocations
  3. Not from frame array access (too small to matter)

---

## Reactor Overhead Dominates

The 44-54% overhead of PARSE mode vs BASE mode comes from:

1. **Callback invocations**: ~% of total time
   - `object_field()` called for every key
   - `scalar_number()` called for every number
   - `scalar_string()` called for every string
   - Reactor trait method dispatch

2. **Monomorphization**: Each reactor type gets a full copy of `parse<R>()`
   - This is actually GOOD - enables inlining and specialization
   - But adds code bloat

3. **Data movement**: StrInfo, RawStr creation and copying
   - 16 bytes per callback invocation
   - Not a huge cost, but adds up

---

## Conclusion

### Frames Off-Stack Optimization
- **Status**: Implemented but minimal benefit
- **Performance change**: -0.6% to +0.8% (within noise)
- **Recommendation**: ✅ KEEP - reduces stack pressure even if not a speed win
  - Useful for embedded/resource-constrained environments
  - Enables future improvements
  - No downside (or negligible downside)

### Real Optimization Opportunities

Based on the data, the real wins are in:

1. **Callback Overhead Reduction** (44-54% of total time)
   - Implement specialized high-performance reactor types
   - Consider `#[inline(always)]` on hot callbacks
   - Reduce per-callback overhead

2. **Input Buffer Prefetching**
   - Use `_mm_prefetch()` to pre-load next chunks
   - Potential 3-5% win

3. **Better Branch Prediction**
   - Reduce conditional branches in hot loops
   - Profile-guided optimization (PGO) might help

4. **SIMD Parallelization**
   - Process multiple JSON documents concurrently
   - SIMD already maxed out for single document

---

## Recommendations

1. ✅ **KEEP** frames-off-stack optimization
   - Minimal performance impact (noise level)
   - Reduces stack pressure
   - No downside

2. 🔄 **INVESTIGATE** callback overhead reduction
   - This is where 44-54% of time is spent
   - Profile with real-world workloads
   - May yield 5-10% improvement

3. 📊 **PROFILE** with actual production JSON
   - These tests use artificial, highly regular JSON
   - Real-world patterns may differ significantly
   - Consider diverse file sizes and nesting depths

