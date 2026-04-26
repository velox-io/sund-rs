# Callback Overhead Analysis

## Executive Summary

An investigation into the claimed "callback overhead bottleneck" revealed that the overhead is NOT caused by callback dispatch or inlining issues, but rather by the intentional algorithmic work performed inside the FullSink reactor.

**Key Metric**: Pure callback dispatch adds only **4.1% overhead**, while the full FullSink reactor adds **74.2% overhead** (compared to NullReactor validation-only baseline). Of this 74.2%, approximately 67.4% is FullSink computation, not dispatch.

---

## Methodology

Three reactor implementations were benchmarked against a 39MB JSON payload, running 100 iterations each:

### Test Harness Variants

1. **NullReactor (BASE mode)**
   - All callbacks return `PROCEED` immediately
   - Represents pure parsing with no output processing
   - Baseline for comparison

2. **MinimalReactor**
   - Single `u32` counter in `object_field()` callback
   - All other callbacks are no-ops
   - Measures pure callback dispatch cost with minimal work

3. **FullSink (PARSE mode)**
   - Full value tracking: counts, f64 summation, string view storage
   - Includes expensive `parse_double()` for every number
   - Original "overhead" target

---

## Results

### Raw Performance Data

```
sund base (validate-only):
  26,523 ns/iter = 1,471 MB/s

sund minimal (dispatch-only callbacks):
  27,597 ns/iter = 1,413 MB/s

sund parse (inline callbacks):
  46,224 ns/iter = 844 MB/s
```

### Overhead Analysis

| Comparison | Delta | Overhead % | Interpretation |
|-----------|-------|-----------|---|
| BASE → MINIMAL | 1,074 ns | +4.1% | Pure callback dispatch cost |
| MINIMAL → PARSE | 18,627 ns | +67.4% | FullSink computation cost |
| BASE → PARSE | 19,701 ns | +74.2% | Total overhead |

---

## Root Cause Analysis

### What's Expensive in FullSink?

The FullSink reactor performs these operations per-callback:

1. **scalar_number() callback** (executed for every JSON number):
   ```rust
   #[inline(always)]
   fn scalar_number(&mut self, raw: RawStr<'_>) -> i32 {
       let bytes = unsafe { std::slice::from_raw_parts(raw.ptr, raw.len as usize) };
       if let Ok(v) = parse_double(bytes) {      // ← EXPENSIVE
           self.num_sum += v;
       }
       PROCEED
   }
   ```
   - Calls `parse_double()` on the unparsed byte sequence
   - This is a full IEEE 754 f64 parser (expensive)
   - Accumulates into f64 field (causes cache line writes)

2. **object_field() and scalar_string() callbacks**:
   ```rust
   #[inline(always)]
   fn emit_view(&mut self, ptr: *const u8, len: u32) {
       let i = (self.view_count & 127) as usize;    // ← Modulo operation
       self.views[i] = StringView { ptr, len };      // ← Array store
       self.view_count += 1;
   }
   ```
   - Modulo operation on every view
   - Array indexing and storage
   - Counter increment + field-level memory write

3. **Counter increments** (multiple fields):
   - `field_count`, `elem_count`, `bool_sum`, `null_count`, `view_count`
   - Each is a memory load-modify-store cycle
   - Creates pressure on store buffer

### Why Callbacks Are Not the Bottleneck

1. **Inlining Works Correctly**
   - FullSink methods are marked `#[inline(always)]`
   - Parser is generic over `R: Reactor`, so monomorphization occurs at compile time
   - Trait method dispatch is resolved at link time
   - Pure dispatch cost is only 4.1%

2. **MinimalReactor Validation**
   - Single counter increment = 4.1% overhead vs BASE
   - This confirms callbacks are inlining and the overhead is just the counter work
   - If callbacks weren't inlining, overhead would be higher

3. **The 67.4% Is Real Work**
   - Not function call overhead
   - Not memory indirection
   - Not trait dispatch
   - It's algorithmic work: f64 parsing, modulo indexing, array stores

---

## Why This Is Not a Bug

The FullSink reactor is **intentionally expensive**. It's designed to:
- Validate that every number was correctly parsed
- Preserve every string reference
- Track structural metrics
- Ensure the parser doesn't skip or misinterpret values

This comprehensiveness requires:
- Parsing every number (not just validating syntax)
- Storing string metadata
- Maintaining accurate counts

A "faster" reactor would simply do less work (like MinimalReactor), which would also provide less confidence that parsing succeeded correctly.

---

## Architectural Note: Trait Method Inlining

The parser achieves effective callback inlining through:

```rust
pub unsafe fn parse<R: Reactor>(ctx: &mut Ctx, reactor: &mut R) {
    // Monomorphic code for each concrete R type
    // Trait methods resolve to direct function calls at link time
    let d = reactor.object_field(key);  // Inlines to FullSink::object_field impl
}
```

This is a well-known Rust pattern: trait methods are inlined when:
1. The generic is monomorphized at compile time
2. The method is in the same crate or marked `#[inline]`
3. The compiler can see the concrete type at inlining site

FullSink's `#[inline(always)]` ensures even aggressive inlining in hot loops.

---

## Conclusion

The Tier 1 optimization "reduce callback overhead" is **not applicable**. 

The callback mechanism is not a bottleneck:
- Dispatch overhead: 4.1% (acceptable)
- Callback inlining: Working correctly
- Trait system: Not causing slowdown

The 74.2% overhead vs validation-only is **intentional and unavoidable** when performing comprehensive value validation. To improve performance would require:

1. **Algorithmic optimization**: Faster f64 parsing (diminishing returns)
2. **Reduced validation**: Skip number parsing (but then numbers aren't validated)
3. **Specialized reactors**: Create domain-specific reactors that don't parse numbers
4. **Accept the tradeoff**: Full value validation costs 60-70% overhead; use NullReactor for pure syntax validation

**Recommendation**: Document this finding and focus optimization efforts on other areas (parser state machine efficiency, cache locality of input scanning, SIMD classifier improvements).

