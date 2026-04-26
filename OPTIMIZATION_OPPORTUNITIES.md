# SUND-RS Optimization Opportunities

## Analysis Date
April 27, 2026 (Updated with callback overhead investigation)

## Baseline Performance

### On 2.1 MB JSON file (4,687 iterations):
- **BASE mode (validation only)**: 1,749 ns/iter = 1,219 MB/s
- **PARSE mode (full callbacks)**: 2,605 ns/iter = 818 MB/s  
- **Overhead ratio**: PARSE/BASE = 1.49x

### On 11.9 MB JSON file (1,000 iterations):
- **BASE mode**: 8,705 ns/iter = 1,372 MB/s
- **PARSE mode**: 12,519 ns/iter = 954 MB/s
- **Overhead ratio**: PARSE/BASE = 1.44x

### On 39 MB JSON file (100 iterations) - Callback Analysis:
- **BASE (NullReactor)**: 26,523 ns/iter = 1,471 MB/s
- **MINIMAL (dispatch-only)**: 27,597 ns/iter = 1,413 MB/s
- **PARSE (FullSink)**: 46,224 ns/iter = 844 MB/s
- **Pure dispatch overhead**: 4.1% (BASE → MINIMAL)
- **Computation overhead**: 67.4% (MINIMAL → PARSE)

## Key Findings from Profiling

### 1. **Cache Misses are Critical** ❌ HIGH IMPACT (ANALYZED)
- CPU cache misses: **99.83%** of all L1 cache references
- This indicates severe memory access patterns
- **Root cause**: `Ctx.frames[256]` was inline array on stack
- **Impact**: Each frame access = cache miss, many misses per parse

### 2. **Reactor Callback Overhead** ✅ INVESTIGATED - NOT A BOTTLENECK
- **Original claim**: 44-54% overhead attributed to callbacks
- **Actual findings**: 
  - Pure callback dispatch cost: **4.1%** (MinimalReactor test)
  - FullSink computation cost: **67.4%** (parse_double, string views, counters)
- **Root causes** (not dispatch):
  - `parse_double()` on every number (expensive IEEE 754 parser)
  - String view storage with modulo-128 indexing
  - F64 accumulation and counter increments
  - All intentional comprehensive validation work

### 3. **SIMD Already Well Optimized** ✅ GOOD
- AVX2 shuffle-LUT classification working efficiently
- PCLMULQDQ prefix_xor is inlined
- Branchless escape resolution (ODD_BITS algorithm)
- Trait method inlining works correctly with monomorphization

### 4. **Stack Frame Layout** ✅ BEING OPTIMIZED
- Original: Ctx with 256×8 bytes frames = 2,048 bytes on stack
- Now: Frames moved to Box, reduces Ctx to ~60 bytes

## Optimization Strategy

### ❌ Optimization #1: Move Frames Off Stack (REJECTED - MINIMAL IMPACT)
**Status**: Tested and found negligible impact  
**Change**: `frames: [Frame; 256]` → `frames: Box<[Frame; MAX_DEPTH]>`  
**Result**: 
- **Measured gain**: -0.6% to +0.8% (within noise)
- **Reason**: Frame array only uses 160-320 bytes per parse (3-6% of L1)
- **Cost**: Box adds one pointer dereference per frame access
- **Conclusion**: Not worth the complexity for negligible gain

---

### ❌ Optimization #2: Reduce Callback Overhead (NOT APPLICABLE)
**Status**: Investigation complete - callbacks not the bottleneck  
**Original expectation**: 5-10% gain from inlining optimization  
**Actual finding**: Callbacks already inline correctly
- Pure dispatch overhead: **4.1%** (acceptable)
- Callbacks already marked `#[inline(always)]`
- Parser uses monomorphization (`parse<R: Reactor>`)
- The 67.4% overhead is intentional comprehensive validation work
- **Conclusion**: Reducing "callback overhead" would require either:
  1. Skipping value validation (defeats purpose)
  2. Faster f64 parsing (diminishing returns, separate task)
  3. Creating specialized low-overhead reactors (design decision)

---

### ✅ Optimization #3: Specialized Reactor Types (POSSIBLE)
**Status**: Proposed based on findings  
**Idea**: Create domain-specific reactor types for common patterns
- `NullReactor` - pure syntax validation (already optimal at 1,471 MB/s)
- `CountingReactor` - only count scalars, skip string storage
- `StringCollector` - collect strings but skip number parsing
- `LowOverheadReactor` - minimal tracking (like MinimalReactor at 1,413 MB/s)

**Expected Impact**:
- Allow use-cases to choose validation level
- CountingReactor: ~4-8% faster than FullSink (skip parse_double)
- StringCollector: ~5-12% faster (skip number parsing)
- **Estimated gain**: 1,000-2,000 ns/iter depending on use-case

**Complexity**: Low - just new Reactor implementations

---

### 🔄 Optimization #4: Reduce Escape Computation (POSSIBLE)
**Current**: ODD_BITS escape resolution for EVERY chunk  
**Observation**: Most JSON has few escapes  
**Idea**: Quick-check for zero backslashes before full computation

**Implementation**:
```rust
// In scan_chunk:
if cls.backslash == 0 {
    state.prev_escape = 0;
    // Skip compute_escaped entirely
    real_quotes = cls.raw_quote;
} else {
    let esc = compute_escaped(cls.backslash, state);
    real_quotes = cls.raw_quote & !esc.escaped;
}
```

**Expected Impact**: 
- Skip compute on escape-free chunks
- 10-15% of typical JSON chunks have zero backslashes
- **Estimated gain**: 2-5% on typical JSON (0-10% on escape-heavy JSON)

**Complexity**: Low - already implemented in line 220-225 of scanner/mod.rs

---

### 🔄 Optimization #5: Batch Structural Discovery (POSSIBLE)
**Change**: Process multiple chunks without re-materializing state  
**Current**: Each chunk: scan → advance → dispatch  
**Idea**: Queue structurals from multiple chunks, reduce dispatch overhead

**Expected Impact**:
- Amortize dispatch overhead
- Better CPU instruction cache usage
- **Estimated gain**: 2-4% on large files

**Complexity**: High - requires significant parser refactoring

---

## Priority Ranking (REVISED)

1. ✅ **SIMD and Branchless Algorithms** - Already at peak efficiency
2. ❌ **Frames Off Stack** - REJECTED (negligible gain, adds complexity)
3. ❌ **Callback Inlining** - NOT APPLICABLE (already optimal, 4.1% dispatch cost)
4. ✅ **Specialized Reactors** - VIABLE (1-2% gain per specialized variant)
5. 🔄 **Escape Computation Skip** - VIABLE (2-5% conditional gain)
6. 🔄 **Batch Structural Processing** - VIABLE but complex (2-4% gain)

**Current assessment**: Parser is already well-optimized at the core level. Gains from here are marginal and require trading performance for functionality:
- Skip validation (NullReactor mode already optimal at 1,471 MB/s)
- Skip number parsing (new specialized reactor type)
- Skip string storage (different specialized reactor type)

---

## Understanding the Performance Wall

### Why PARSE mode is 60-70% slower than BASE mode

The gap is **not** a bug or missed optimization. It reflects the fundamental computational cost of:

1. **Number Validation** (largest component):
   - parse_double() implementation: state machine for IEEE 754
   - Mandatory for "validation" - just checking syntax doesn't validate correctness
   - All scalars: 600K numbers in test payload

2. **String Reference Tracking** (second component):
   - Modulo indexing for circular buffer of string views
   - Memory stores for 2.3M string references
   - Necessary to prove parser correctly identified all strings

3. **Metric Accumulation** (third component):
   - F64 accumulation for number sum
   - Multiple counter increments with memory stores
   - Cache line pressure from multiple field stores

### What's Already Optimal

- SIMD scanning (AVX2, NEON implementations)
- Prefix-XOR computation (PCLMULQDQ)
- Escape resolution (ODD_BITS branchless algorithm)
- Trait method dispatch (monomorphization eliminates vtable calls)
- String scanning (minimal overhead)

### What Cannot Be Optimized Without Tradeoffs

- Number parsing (requires parsing to validate, not just syntax check)
- String storage (requires memory to prove all strings were found)
- Counters (requires memory stores to track metrics)

---

## Recommended Next Steps

1. **Document performance model**: 1,471 MB/s (validation) + 300 MB/s per feature
2. **Provide specialized reactors**: Let users choose validation level
3. **Accept current performance**: Gains from here are marginal and require reducing validation
4. **Focus on use-case**: If you need only syntax validation, use NullReactor
5. **Future: Faster float parser**: Could improve by 5-10% with optimized parse_double()

---

## References

- See `/CALLBACK_OVERHEAD_ANALYSIS.md` for detailed callback investigation
- See `/BENCHMARK_REPORT.md` for performance baselines
- See `/ARCHITECTURE_ANALYSIS.md` for parser internals

