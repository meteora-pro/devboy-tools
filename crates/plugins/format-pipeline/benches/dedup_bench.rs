//! Benchmark: `DedupCache` hot-path operations.
//!
//! Measures per-call latency of the dedup cache under realistic workloads:
//!
//! - `content_hash(payload)` — SHA-256 prefix of tool response
//! - `cache.check(&hash)`     — lookup in an N-entry LRU
//! - `cache.insert(...)`      — add a new entry, evicting oldest if full
//! - `cache.invalidate_file`  — drop file-read entries for a modified path
//!
//! Targets: p99 < 100 µs end-to-end even for 8k-char payloads, so a session
//! emitting 200 tool_responses adds < 20 ms aggregate overhead.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use devboy_format_pipeline::dedup::{
    DedupCache, DedupDecision, ToolKind, content_hash, render_reference_hint,
};

/// Build a synthetic tool-response payload of the given length.
fn payload(n: usize) -> String {
    // Mix of alphanumerics + whitespace + braces to stress SHA-256 paths
    // realistically (not just repeated 'a's).
    let template = "{\"id\":12345,\"status\":\"success\",\"logs\":[\"abc\",\"def\"]}\n";
    let mut s = String::with_capacity(n);
    while s.len() < n {
        s.push_str(template);
    }
    s.truncate(n);
    s
}

fn bench_content_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("dedup/content_hash");
    for size in [256usize, 1024, 4096, 16384, 65536] {
        let p = payload(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &p, |b, p| {
            b.iter(|| {
                let h = content_hash(black_box(p.as_bytes()));
                black_box(h);
            });
        });
    }
    group.finish();
}

fn bench_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("dedup/check");
    // Pre-populate caches of various sizes with realistic entries, then
    // time the `check` operation for a miss (worst case: full scan).
    for cap in [1usize, 5, 20, 100] {
        let mut cache = DedupCache::with_capacity(cap);
        for i in 0..cap {
            let h = content_hash(format!("response_{i}").as_bytes());
            cache.insert(
                h,
                format!("tc_{i}"),
                ToolKind::Other,
                None,
            );
        }
        let miss_hash = content_hash(b"miss_payload_never_inserted");
        group.bench_with_input(BenchmarkId::new("miss", cap), &cap, |b, _| {
            b.iter(|| {
                let d = cache.check(black_box(&miss_hash));
                black_box(d);
            });
        });
        // Hit case — fingerprint matches first entry
        let hit_hash = content_hash(b"response_0");
        group.bench_with_input(BenchmarkId::new("hit", cap), &cap, |b, _| {
            b.iter(|| {
                let d = cache.check(black_box(&hit_hash));
                black_box(d);
            });
        });
    }
    group.finish();
}

fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("dedup/insert");
    for cap in [5usize, 20, 100] {
        group.bench_with_input(BenchmarkId::from_parameter(cap), &cap, |b, &cap| {
            b.iter_batched(
                || {
                    // Setup: full cache, about to evict on next insert
                    let mut cache = DedupCache::with_capacity(cap);
                    for i in 0..cap {
                        cache.insert(
                            content_hash(format!("r_{i}").as_bytes()),
                            format!("tc_{i}"),
                            ToolKind::Other,
                            None,
                        );
                    }
                    cache
                },
                |mut cache| {
                    cache.insert(
                        black_box(content_hash(b"new_response")),
                        black_box("tc_new"),
                        black_box(ToolKind::Other),
                        black_box(None),
                    );
                    black_box(cache);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_invalidate_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("dedup/invalidate_file");
    // How fast is a FileMutate invalidation on a cache where the target file
    // is present vs absent?
    for cap in [5usize, 20, 100] {
        group.bench_with_input(BenchmarkId::new("miss", cap), &cap, |b, &cap| {
            b.iter_batched(
                || {
                    let mut cache = DedupCache::with_capacity(cap);
                    for i in 0..cap {
                        cache.insert(
                            content_hash(format!("r_{i}").as_bytes()),
                            format!("tc_{i}"),
                            ToolKind::FileRead,
                            Some(format!("path_{i}")),
                        );
                    }
                    cache
                },
                |mut cache| {
                    let n = cache.invalidate_file(black_box("missing_path"));
                    black_box(n);
                    black_box(cache);
                },
                criterion::BatchSize::SmallInput,
            );
        });
        group.bench_with_input(BenchmarkId::new("hit", cap), &cap, |b, &cap| {
            b.iter_batched(
                || {
                    let mut cache = DedupCache::with_capacity(cap);
                    for i in 0..cap {
                        cache.insert(
                            content_hash(format!("r_{i}").as_bytes()),
                            format!("tc_{i}"),
                            ToolKind::FileRead,
                            Some(format!("path_{i}")),
                        );
                    }
                    cache
                },
                |mut cache| {
                    let n = cache.invalidate_file(black_box("path_0"));
                    black_box(n);
                    black_box(cache);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_end_to_end(c: &mut Criterion) {
    // Realistic pipeline call: hash payload, check cache, insert if fresh,
    // render hint if dup. Targets p99 < 100 µs for 8 KiB payloads.
    let mut group = c.benchmark_group("dedup/end_to_end");
    for size in [512usize, 4096, 32768] {
        let p = payload(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &p, |b, p| {
            b.iter_batched(
                || {
                    let mut cache = DedupCache::with_capacity(5);
                    // Warm cache with 4 distinct entries
                    for i in 0..4 {
                        cache.insert(
                            content_hash(format!("old_{i}").as_bytes()),
                            format!("tc_{i}"),
                            ToolKind::Other,
                            None,
                        );
                    }
                    cache
                },
                |mut cache| {
                    // 1. hash payload
                    let h = content_hash(black_box(p.as_bytes()));
                    // 2. check
                    let decision = cache.check(&h);
                    // 3. emit hint or insert fresh
                    match decision {
                        DedupDecision::Hint { reference_tool_call_id } => {
                            let out = render_reference_hint(&reference_tool_call_id);
                            black_box(out);
                        }
                        DedupDecision::Fresh => {
                            cache.insert(h, "tc_new", ToolKind::Other, None);
                        }
                    }
                    black_box(cache);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_content_hash,
    bench_check,
    bench_insert,
    bench_invalidate_file,
    bench_end_to_end,
);
criterion_main!(benches);
