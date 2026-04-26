# Sund-RS: Quick Reference for Hot Path Optimization

## The Hottest Code Paths (in order of impact)

### 1. **parser.rs: `parse<R>()` main loop (line 45–1182)**
   - **What:** Computed-goto state machine dispatching 9 phases
   - **Where:** Every byte eventually reaches here
   - **Key:** aarch64/x86-64 use inline `asm_goto` labels for 4-instruction dispatch
   - **Micro-level:**
     - Loads `cur_pos`, `chunk_ptr`, `bits` into registers at entry
     - Saves only on exit → ~4 loads, ~4 stores per parse call
     - Inlines `next_structural!()`, `parse_string_span!()`, `parse_number_span!()` macros

### 2. **scanner/mod.rs: `scan_chunk()` (line 216–248)**
   - **What:** Transforms 64 input bytes → 1 structural bitmap
   - **Where:** Called once per 64 bytes
   - **Cost:** ~1–2 cycles per byte (AVX2) or ~4 cycles per 16 bytes (NEON)
   - **Pipeline:** 
     1. `classify_chunk()` → backslash/quote/whitespace/op bitmaps
     2. `compute_escaped()` → ODD_BITS algorithm (branchless!)
     3. `prefix_xor()` → SIMD carry-less multiply or scalar shift-cascade
     4. Merge → output structural bitmap
   - **Inlined:** Yes, with architecture-specific features (avx2, pclmulqdq on x86)

### 3. **scanner/avx2.rs or neon.rs: `classify_chunk()` (line 50–120 or 82–140)**
   - **What:** SIMD byte-by-byte classification via shuffle-LUTs
   - **x86-64:** 2×AVX2 loads + 3×shuffle + 3×compare = ~10 instructions
   - **aarch64:** 4×NEON operations, dual-LUT approach
   - **Inlined:** Yes, into scan_chunk() when called from AVX2 context
   - **Bottleneck:** Memory-bound; achieves ~5 GB/s max on modern x86

### 4. **parser.rs: `next_structural!()` macro (line 177–198)**
   - **What:** Iterate through structural bits; refill chunk on empty
   - **Called:** ~100s–1000s of times per JSON
   - **Cost per call:** ~3 instructions (tzcnt + test + conditional jump) when bits != 0
   - **Trick:** `ctz64_empty()` uses single `trailing_zeros()` + compare; avoids asm pessimism
   - **Inlined:** Via macro; body lives in parse loop

### 5. **scanner/mod.rs: `advance_chunk()` (line 305–331)**
   - **What:** Load next 64-byte chunk and scan it
   - **Where:** Called from `next_structural!()` when structural_bits == 0
   - **Hot path:** ~64 bytes available → direct `scan_chunk()` call
   - **Cold path:** Tail chunk (<64 bytes) → outline to `advance_chunk_tail()`
   - **Inlined:** Yes (despite being called from many sites)
   - **Why no code bloat?** Because cold path is outlined

### 6. **scalar.rs: `string_span()` (line 144–195)**
   - **What:** Find closing `"` while tracking backslash escapes
   - **Called:** Once per string field/value
   - **Cost:** Varies; 1–10 chunks per string (depends on string length)
   - **Algorithm:** 
     - Loop: scan `bits` for next structural
     - If quote found → return
     - If not → `advance_chunk()` and loop
   - **Optimization:** Keeps `bs_bits` in sync for escape detection (ODD_BITS)

### 7. **scanner/mod.rs: `compute_escaped()` (line 181–206)**
   - **What:** Branchless backslash/escape resolution (simdjson algorithm)
   - **Formula:** ODD_BITS = 0xAAAA_AAAA_AAAA_AAAA; shift-subtract-XOR cascade
   - **No branches:** All bit arithmetic; no loops
   - **Called:** Every chunk where backslash appears
   - **Cost:** ~12 instructions

### 8. **scanner/mod.rs: `prefix_xor()` (line 124–142)**
   - **What:** Compute prefix-XOR (determines in-string regions)
   - **x86-64:** PCLMULQDQ carry-less multiply (3 instructions)
   - **aarch64:** PMULL polynomial multiply (3 instructions)
   - **Fallback:** Shift-XOR cascade (6 shifts + 6 XORs = ~12 instructions)
   - **Called:** Once per chunk
   - **Inlined:** Yes, when called from AVX2/NEON context

---

## Benchmark Modes

```bash
# Build release
cargo build --release -p sund_json

# Benchmark validate-only (NullReactor, zero reactor overhead)
./benches/sund-bench/target/release/sund_bench base

# Benchmark full parse with callbacks (FullSink reactor)
./benches/sund-bench/target/release/sund_bench parse

# Both (sequentially)
./benches/sund-bench/target/release/sund_bench

# With custom payload
./benches/sund-bench/target/release/sund_bench --payload=/path/to/file.json

# Auto-tuned to ~2 seconds; override with env var
BENCH_ITERS=1000000 ./benches/sund-bench/target/release/sund_bench base
```

---

## Key Optimizations Already in Place

| Optimization | Location | Benefit |
|--------------|----------|---------|
| **Computed-goto dispatch** | parser.rs lines 1082–1146 | Replaces dense match; 4-instruction jump table |
| **SIMD classification** | scanner/*.rs | 64 bytes → bitmap in ~1–4 cycles |
| **ODD_BITS escape** | scanner/mod.rs line 181–206 | Branchless backslash resolution |
| **Branchless ctz64_empty** | scanner/mod.rs line 105–108 | Single compare (not asm pessimism) |
| **Cross-chunk carry** | types.rs ScanState | No re-scanning input boundaries |
| **Inline macros** | parser.rs | Hot bits stay in registers |
| **Outlined cold path** | scanner/mod.rs line 267–286 | Tail chunk logic doesn't bloat hot loop |
| **Fat LTO + codegen-units=1** | root Cargo.toml | Inter-procedural optimization across crates |
| **Generic reactor** | parser.rs generics | Monomorphizes: NullReactor has zero callback cost |

---

## File Map: What Lives Where

| File | Size | Hot Paths | Purpose |
|------|------|-----------|---------|
| `parser.rs` | 49 KB | ⭐⭐⭐ parse, next_structural! | State machine |
| `scanner/mod.rs` | 14 KB | ⭐⭐⭐ scan_chunk, advance_chunk | Bitmap generation |
| `scanner/avx2.rs` | 4.8 KB | ⭐⭐⭐ classify_chunk | AVX2 SIMD |
| `scanner/neon.rs` | 4.8 KB | ⭐⭐⭐ classify_chunk | ARM NEON |
| `scalar.rs` | 7.6 KB | ⭐⭐ string_span, number_span | Span finding |
| `types.rs` | 7.3 KB | ⭐ Ctx, ScanState | Structures |
| `reactor.rs` | 2.4 KB | ⭐⭐ callbacks | Trait + NullReactor |
| `stream.rs` | 4.4 KB | ⭐ feed() | Streaming wrapper |

---

## Hot Loop Pseudocode

```rust
// Simplified parse loop
pub unsafe fn parse<R: Reactor>(ctx: &mut Ctx, reactor: &mut R) {
    // Load hot fields into registers
    let mut cur_pos = ctx.cur_pos;
    let mut chunk_ptr = ctx.chunk_ptr;
    let mut bits = ctx.structural_bits;
    let mut scan_state = ctx.scan_state;
    
    loop {
        // Dispatch via computed-goto (9 phases)
        match current_phase {
            PH_ROOT_VALUE => {
                // Get next structural: tzcnt if bits != 0, else advance_chunk
                let ch = next_structural!();
                
                match ch {
                    b'{' => { reactor.begin_object(); ... }
                    b'[' => { reactor.begin_array(); ... }
                    b'"' => { string_span!(); reactor.scalar_string(); ... }
                    _ => { ... }
                }
            }
            // ... 8 more phases ...
        }
    }
}

// expand next_structural!() to:
//   loop {
//       if bits != 0 {
//           idx = bits.trailing_zeros();      // tzcnt
//           cur_pos = chunk_ptr.add(idx);
//           bits = clear_lowest_bit(bits);
//           break *cur_pos as i32;
//       }
//       let ar = advance_chunk(chunk_ptr, buf_end, state);
//       if ar.chunk_ptr == chunk_ptr { break EOF; }
//       chunk_ptr = ar.chunk_ptr;
//       bits = ar.bits;
//   }
```

---

## Profiling Entry Points

If you need to profile:

1. **Overall throughput:** 
   - Run `cargo run --release -p sund_bench base` 
   - Reports ns/iter, MB/s, GB/s

2. **Assembly inspection:**
   ```bash
   cargo build --release -p sund_json
   objdump -d target/release/deps/sund_json-*.so | grep -A 500 "parse>:"
   ```

3. **Perf record:**
   ```bash
   perf record -g ./target/release/sund_bench base
   perf report
   ```

4. **Flamegraph (if available):**
   ```bash
   cargo flamegraph --release --bin sund_bench
   ```

---

## Cargo.toml Settings Explained

### Workspace root (opt-level, LTO, codegen-units)

```toml
[profile.release]
opt-level = 3        # Maximum optimization (-O3)
lto = "fat"          # Whole-program LTO (slow compile, best code)
codegen-units = 1    # Single unit (enables inter-procedural optimizations)
```

**Effect:** Parser gets aggressive inlining, better branch prediction, tighter code.

### Benchmark build

```toml
[profile.release]
opt-level = 3        # Same
lto = "thin"         # Faster LTO link time (benchmark doesn't need fat)
codegen-units = 1    # Same
```

---

## State Machine Diagram (9 Phases)

```
                     ┌─────────────────────────────┐
                     │    ROOT_VALUE (phase 0)     │
                     │ Expect { [ or scalar        │
                     └──────────┬──────────────────┘
                                │
                 ┌──────────────┴──────────────────┐
                 │                                 │
                 v                                 v
        ┌──────────────────┐           ┌──────────────────┐
        │ OBJ_FIELD_OR_END │           │ ARR_ELEM_OR_END  │
        │ Expect " or }    │           │ Expect value or ]│
        └────────┬─────────┘           └────────┬─────────┘
                 │                              │
                 v                              v
        ┌──────────────────┐           ┌──────────────────┐
        │ OBJ_FIELD_VALUE  │           │ ARR_ELEM_VALUE   │
        │ Parse value      │           │ Parse value      │
        └────────┬─────────┘           └────────┬─────────┘
                 │                              │
                 v                              v
        ┌──────────────────┐           ┌──────────────────┐
        │ OBJ_CONTINUE     │           │ ARR_CONTINUE     │
        │ Expect , or }    │           │ Expect , or ]    │
        └────────┬─────────┘           └────────┬─────────┘
                 └──────────────────┬───────────┘
                                    │
                                    v
                          ┌──────────────────┐
                          │   ROOT_DONE      │
                          │ Expect EOF       │
                          └──────────────────┘

Reactor returns SKIP → enters PH_SKIP_VALUE (depth tracking for containers)
```

---

## Optimization Checklist for Profiling

- [ ] Run `sund_bench base` → confirm ~5–6 GB/s throughput
- [ ] Run `sund_bench parse` → confirm ~3–4 GB/s (reactor overhead ~40%)
- [ ] Check branch prediction: `perf stat -e branch-misses ./sund_bench base`
- [ ] Check cache: `perf stat -e LLC-load-misses ./sund_bench base`
- [ ] Profile hot functions: `perf record -g ./sund_bench base && perf report`
- [ ] Inspect final binary: `objdump -d target/release/libsund_json.so | grep -C 20 "scan_chunk"`
- [ ] Measure LTO impact: compare release build with/without `lto = "fat"`
- [ ] Check inlining: `rustc -C llvm-args=-print-after=inline target/release/sund_json.ll 2>&1 | grep -A 5 parse`

