//! sund_bench — benchmark suite matching ndec_bench.c
//!
//! Supports two sub-tests:
//!   ./sund_bench base    — validate-only (NullReactor, zero reactor work)
//!   ./sund_bench parse   — full parse with inline callbacks (FullSink reactor)
//!   ./sund_bench         — run both sequentially
//!
//! Payload resolution (first match):
//!   1. explicit --payload=<path> flag
//!   2. $SUND_BENCH_PAYLOAD or $NDEC_BENCH_PAYLOAD env var
//!   3. hard-coded fallback path
//!
//! Environment:
//!   BENCH_ITERS=<n>  override iteration count (default: auto-tuned to ~2s)

use std::env;
use std::fs;
use std::hint::black_box;
use std::time::Instant;

use sund_json::parser;
use sund_json::reactor::{NullReactor, Reactor};
use sund_json::types::*;

use sund_json::bind::number::parse_double;

/// Default payload paths to try (relative to CWD or absolute).
const DEFAULT_PATHS: &[&str] = &[
    "test/data/bench_payload.json",
    "benches/data/bench_payload.json",
    "../velox-json-work/native/ndec/test/data/bench_payload.json",
];

fn load_payload() -> (Vec<u8>, String) {
    // 1. Check --payload=<path> in args
    for arg in env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--payload=") {
            let data = fs::read(path).unwrap_or_else(|e| {
                eprintln!("sund_bench: cannot read {}: {}", path, e);
                std::process::exit(1);
            });
            return (data, path.to_string());
        }
    }

    // 2. Check env vars
    for var in &["SUND_BENCH_PAYLOAD", "NDEC_BENCH_PAYLOAD"] {
        if let Ok(path) = env::var(var) {
            if !path.is_empty() {
                let data = fs::read(&path).unwrap_or_else(|e| {
                    eprintln!("sund_bench: cannot read {} (from ${}): {}", path, var, e);
                    std::process::exit(1);
                });
                return (data, path);
            }
        }
    }

    // 3. Positional arg that's not a mode keyword
    for arg in env::args().skip(1) {
        if arg != "base" && arg != "parse" && !arg.starts_with("--") {
            let data = fs::read(&arg).unwrap_or_else(|e| {
                eprintln!("sund_bench: cannot read {}: {}", arg, e);
                std::process::exit(1);
            });
            return (data, arg);
        }
    }

    // 4. Try default paths
    for path in DEFAULT_PATHS {
        if let Ok(data) = fs::read(path) {
            return (data, path.to_string());
        }
    }

    eprintln!("sund_bench: no payload found. Use --payload=<path> or set $SUND_BENCH_PAYLOAD");
    std::process::exit(1);
}

fn get_iterations(json_len: usize) -> usize {
    if let Ok(s) = env::var("BENCH_ITERS") {
        return s.parse().unwrap_or(1_000_000);
    }
    // Auto-tune: target ~2 seconds of work.
    // Rough estimate: ~5 GB/s throughput → ns_per_byte ≈ 0.2
    // iterations = 2e9 ns / (json_len * 0.2)
    let target_ns: f64 = 2_000_000_000.0;
    let est_ns_per_byte: f64 = 0.2;
    let est = (target_ns / (json_len as f64 * est_ns_per_byte)) as usize;
    est.max(1000).min(50_000_000)
}

fn bench_base(json: &[u8]) {
    let json_len = json.len();
    let iterations = get_iterations(json_len);

    eprintln!("JSON payload size: {} bytes", json_len);
    eprintln!("Running {} iterations (BASE mode)...", iterations);

    // Warmup
    for _ in 0..1000 {
        let mut ctx = Ctx::new();
        ctx.set_input_slice(json, true);
        unsafe { parser::parse(&mut ctx, &mut NullReactor) };
        black_box(&ctx);
    }

    // Benchmark
    let start = Instant::now();
    for _ in 0..iterations {
        let mut ctx = Ctx::new();
        ctx.set_input_slice(json, true);
        unsafe { parser::parse(&mut ctx, &mut NullReactor) };
        black_box(&ctx);
    }
    let elapsed = start.elapsed();

    let ns_per_iter = elapsed.as_nanos() as f64 / iterations as f64;
    let mb_per_sec = json_len as f64 * iterations as f64 / elapsed.as_secs_f64() / 1e6;
    let gb_per_sec = mb_per_sec / 1000.0;

    eprintln!("Done.\n");
    println!("sund base (validate-only):");
    println!("  {} iterations, {} bytes each", iterations, json_len);
    println!("  {:.1} ns/iter", ns_per_iter);
    println!("  {:.1} MB/s ({:.2} GB/s)", mb_per_sec, gb_per_sec);
}

#[derive(Clone, Copy)]
struct StringView {
    ptr: *const u8,
    len: u32,
}

struct FullSink {
    num_sum: f64,
    field_count: u32,
    elem_count: u32,
    bool_sum: u32,
    null_count: u32,
    view_count: u32,
    views: [StringView; 128],
}

impl FullSink {
    fn new() -> Self {
        Self {
            num_sum: 0.0,
            field_count: 0,
            elem_count: 0,
            bool_sum: 0,
            null_count: 0,
            view_count: 0,
            views: [StringView {
                ptr: std::ptr::null(),
                len: 0,
            }; 128],
        }
    }

    fn reset(&mut self) {
        self.num_sum = 0.0;
        self.field_count = 0;
        self.elem_count = 0;
        self.bool_sum = 0;
        self.null_count = 0;
        self.view_count = 0;
    }

    #[inline(always)]
    fn emit_view(&mut self, ptr: *const u8, len: u32) {
        let i = (self.view_count & 127) as usize;
        self.views[i] = StringView { ptr, len };
        self.view_count += 1;
    }
}

impl Reactor for FullSink {
    #[inline(always)]
    fn begin_object(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn end_object(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn object_field(&mut self, key: StrInfo<'_>) -> i32 {
        self.emit_view(key.raw.ptr, key.raw.len);
        self.field_count += 1;
        PROCEED
    }

    #[inline(always)]
    fn begin_array(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn end_array(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn array_elem(&mut self) -> i32 {
        self.elem_count += 1;
        PROCEED
    }

    #[inline(always)]
    fn scalar_null(&mut self) -> i32 {
        self.null_count += 1;
        PROCEED
    }

    #[inline(always)]
    fn scalar_bool(&mut self, value: bool) -> i32 {
        self.bool_sum += value as u32;
        PROCEED
    }

    #[inline(always)]
    fn scalar_number(&mut self, raw: RawStr<'_>) -> i32 {
        let bytes = unsafe { std::slice::from_raw_parts(raw.ptr, raw.len as usize) };
        if let Ok(v) = parse_double(bytes) {
            self.num_sum += v;
        }
        PROCEED
    }

    #[inline(always)]
    fn scalar_string(&mut self, s: StrInfo<'_>) -> i32 {
        self.emit_view(s.raw.ptr, s.raw.len);
        PROCEED
    }
}

fn bench_parse(json: &[u8]) {
    let json_len = json.len();
    let iterations = get_iterations(json_len);

    eprintln!("JSON payload size: {} bytes", json_len);
    eprintln!("Running {} iterations (PARSE mode)...", iterations);

    let mut sink = FullSink::new();

    // Warmup
    for _ in 0..1000 {
        sink.reset();
        let mut ctx = Ctx::new();
        ctx.set_input_slice(json, true);
        unsafe { parser::parse(&mut ctx, &mut sink) };
        black_box(&ctx);
        black_box(&sink.num_sum);
    }

    // Benchmark
    let start = Instant::now();
    for _ in 0..iterations {
        sink.reset();
        let mut ctx = Ctx::new();
        ctx.set_input_slice(json, true);
        unsafe { parser::parse(&mut ctx, &mut sink) };
        black_box(&ctx);
        black_box(&sink.num_sum);
    }
    let elapsed = start.elapsed();

    let ns_per_iter = elapsed.as_nanos() as f64 / iterations as f64;
    let mb_per_sec = json_len as f64 * iterations as f64 / elapsed.as_secs_f64() / 1e6;
    let gb_per_sec = mb_per_sec / 1000.0;

    eprintln!("Done.\n");
    println!("sund parse (inline callbacks):");
    println!("  {} iterations, {} bytes each", iterations, json_len);
    println!("  {:.1} ns/iter", ns_per_iter);
    println!("  {:.1} MB/s ({:.2} GB/s)", mb_per_sec, gb_per_sec);
    eprintln!(
        "sink.num_sum={:.1} views={} fields={} elems={} bool={} null={}",
        sink.num_sum,
        sink.view_count,
        sink.field_count,
        sink.elem_count,
        sink.bool_sum,
        sink.null_count
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let run_base =
        args.iter().any(|a| a == "base") || (!args.iter().any(|a| a == "parse" || a == "minimal"));
    let run_parse =
        args.iter().any(|a| a == "parse") || (!args.iter().any(|a| a == "base" || a == "minimal"));
    let run_minimal = args.iter().any(|a| a == "minimal");

    let (json, path) = load_payload();
    eprintln!("sund_bench: loaded {} bytes from {}", json.len(), path);

    if run_base {
        bench_base(&json);
        if run_parse || run_minimal {
            println!();
        }
    }

    if run_parse {
        bench_parse(&json);
        if run_minimal {
            println!();
        }
    }

    if run_minimal {
        bench_minimal(&json);
    }
}

/// Minimal reactor for measuring dispatch overhead.
struct MinimalReactor {
    count: u32,
}

impl MinimalReactor {
    fn new() -> Self {
        Self { count: 0 }
    }

    fn reset(&mut self) {
        self.count = 0;
    }
}

impl Reactor for MinimalReactor {
    #[inline(always)]
    fn begin_object(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn end_object(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn object_field(&mut self, _key: StrInfo<'_>) -> i32 {
        self.count += 1;
        PROCEED
    }

    #[inline(always)]
    fn begin_array(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn end_array(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn array_elem(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn scalar_null(&mut self) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn scalar_bool(&mut self, _value: bool) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn scalar_number(&mut self, _raw: RawStr<'_>) -> i32 {
        PROCEED
    }

    #[inline(always)]
    fn scalar_string(&mut self, _s: StrInfo<'_>) -> i32 {
        PROCEED
    }
}

fn bench_minimal(json: &[u8]) {
    let json_len = json.len();
    let iterations = get_iterations(json_len);

    eprintln!("JSON payload size: {} bytes", json_len);
    eprintln!("Running {} iterations (MINIMAL reactor)...", iterations);

    let mut reactor = MinimalReactor::new();

    // Warmup
    for _ in 0..1000 {
        reactor.reset();
        let mut ctx = Ctx::new();
        ctx.set_input_slice(json, true);
        unsafe { parser::parse(&mut ctx, &mut reactor) };
    }

    let start = Instant::now();
    for _ in 0..iterations {
        reactor.reset();
        let mut ctx = Ctx::new();
        ctx.set_input_slice(json, true);
        unsafe { parser::parse(&mut ctx, &mut reactor) };
    }
    let elapsed = start.elapsed();

    let ns_per_iter = elapsed.as_nanos() as f64 / iterations as f64;
    let mb_per_sec = json_len as f64 * iterations as f64 / elapsed.as_secs_f64() / 1e6;
    let gb_per_sec = mb_per_sec / 1000.0;

    eprintln!("Done.\n");
    println!("sund minimal (dispatch-only callbacks):");
    println!("  {} iterations, {} bytes each", iterations, json_len);
    println!("  {:.1} ns/iter", ns_per_iter);
    println!("  {:.1} MB/s ({:.2} GB/s)", mb_per_sec, gb_per_sec);
    eprintln!("reactor.count={}", reactor.count);
}
