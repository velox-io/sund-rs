# SUND-RS Optimization Opportunities

## Analysis Date
April 27, 2026

## Baseline Performance

### On 2.1 MB JSON file (4,687 iterations):
- **BASE mode (validation only)**: 1,749 ns/iter = 1,219 MB/s
- **PARSE mode (full callbacks)**: 2,605 ns/iter = 818 MB/s  
- **Overhead ratio**: PARSE/BASE = 1.49x

### On 11.9 MB JSON file (1,000 iterations):
- **BASE mode**: 8,705 ns/iter = 1,372 MB/s
- **PARSE mode**: 12,519 ns/iter = 954 MB/s
- **Overhead ratio**: PARSE/BASE = 1.44x

## Key Findings from Profiling

### 1. **Cache Misses are Critical** ❌ HIGH IMPACT
- CPU cache misses: **99.83%** of all L1 cache references
- This indicates severe memory access patterns
- **Root cause**: `Ctx.frames[256]` was inline array on stack
- **Impact**: Each frame access = cache miss, many misses per parse

### 2. **Reactor Callback Overhead** ❌ MEDIUM IMPACT  
- 44-49% of time is reactor overhead (PARSE vs BASE)
- Virtual function calls (inlined but still add overhead)
- Object field, array elem, value callbacks for every parsed item

### 3. **SIMD Already Well Optimized** ✅ GOOD
- AVX2 shuffle-LUT classification working efficiently
- PCLMULQDQ prefix_xor is inlined
- Branchless escape resolution (ODD_BITS algorithm)

### 4. **Stack Frame Layout** ✅ BEING OPTIMIZED
- Original: Ctx with 256×8 bytes frames = 2,048 bytes on stack
- Now: Frames moved to Box, reduces Ctx to ~60 bytes

## Optimization Strategy

### Optimization #1: Move Frames Off Stack ✅ IN PROGRESS
**Status**: Partially implemented  
**Change**: `frames: [Frame; 256]` → `frames: Box<[Frame; MAX_DEPTH]>`  
**Expected Impact**: 
- Reduce stack pressure from 2KB to ~60 bytes
- Improve cache locality of hot fields
- Reduce page faults
- **Estimated gain**: 5-15% on PARSE mode

**Files Modified**:
- `crates/sund-json/src/types.rs` - Changed Ctx struct

**Verification**: ✅ Already built and tested (2,605 ns/iter on 2.1MB)

---

### Optimization #2: Reduce Callback Overhead 🔄 PROPOSED
**Change**: Create specialized reactor types for hot paths  
**Idea**: Monomorphize common reactor patterns:
- `NullReactor` (validate-only) - inline, no work
- `CountingReactor` (counters only) - inline everything
- `StringCollector` (strings only) - optimize string paths

**Expected Impact**: 
- Eliminate virtual dispatch overhead
- Better branch prediction
- Potential inlining of callbacks
- **Estimated gain**: 5-10% on PARSE mode

**Complexity**: Medium - requires creating new reactor types

---

### Optimization #3: Hot Path Inlining 🔄 PROPOSED
**Change**: Mark hot functions with `#[inline(always)]`  
**Targets**:
- `scalar::string_span()` - already inline
- `scalar::number_span()` - already inline  
- `scanner::advance_chunk()` - already inline
- `reactor callbacks` - NOT currently inline

**Expected Impact**: 
- Reduce function call overhead
- Better code cache usage
- **Estimated gain**: 3-8% on PARSE mode

---

### Optimization #4: Reduce Escape Computation 🔄 PROPOSED
**Current**: ODD_BITS escape resolution for EVERY chunk  
**Observation**: Most JSON has few escapes  
**Idea**: Quick-check for zero backslashes before full computation

**Expected Impact**: 
- Skip expensive compute on escape-free chunks
- 10-15% of chunks likely have zero escapes
- **Estimated gain**: 2-5% on typical JSON

---

### Optimization #5: Batch Structural Discovery 🔄 PROPOSED
**Change**: Process multiple chunks without re-materializing state  
**Current**: Each chunk: scan → advance → dispatch  
**Idea**: Queue up N chunks worth of structurals to reduce dispatch overhead

**Expected Impact**:
- Amortize dispatch overhead
- Better CPU cache usage
- **Estimated gain**: 2-4% on large files

---

## Priority Ranking

1. **Frames Off Stack** ✅ DONE - Expected 5-15% gain
2. **Callback Overhead Reduction** - Expected 5-10% gain  
3. **Hot Path Inlining** - Expected 3-8% gain
4. **Reduce Escape Computation** - Expected 2-5% gain
5. **Batch Structural Discovery** - Expected 2-4% gain

**Total potential gain**: 17-42% improvement

---

## Assembly-Level Observations

### Current Hot Loop (from objdump):
```
vpbroadcastb (backslash char)      # 1 instr
vpcmpeqb     (compare)             # 2 instrs (for both 256-bit halves)
vpmovmskb    (extract mask)        # 2 instrs
shl          (shift into position) # 2 instrs  
or           (combine masks)       # 1 instr
andn         (bitwise ops)         # Multiple instrs
```

### Potential Improvements:
- VPMOVMSKB is already optimal (fastest mask extraction)
- Shifts and combines could be reduced by better register allocation
- ODD_BITS computation is already branchless and minimal

---

## Testing Plan

1. ✅ Baseline on main branch (2,133 KB JSON)
2. ✅ Test frames-off-stack optimization branch
3. ☐ Measure improvement
4. ☐ Implement callback optimization
5. ☐ Measure cumulative improvement
6. ☐ Create comprehensive benchmark report

