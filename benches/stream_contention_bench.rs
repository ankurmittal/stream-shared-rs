use criterion::{criterion_group, criterion_main, Criterion};
use futures_util::{future::join_all, stream::StreamExt};
use std::time::Duration;
use stream_shared::SharedStream;
use tokio::runtime::Runtime;

mod utils;
use utils::{
    generate_in_memory_stream, generate_simulated_io_stream, run_channel_fan_out, ArcData,
    NUM_CONSUMERS,
};

// --- Benchmark Runner Function ---

// Function to run the consumption logic for N consumers
async fn consume_streams<S>(streams: Vec<S>)
where
    S: futures_util::stream::Stream<Item = ArcData> + Unpin,
{
    let futures: Vec<_> = streams
        .into_iter()
        .map(|mut stream| async move {
            let mut consumed_count = 0;
            while stream.next().await.is_some() {
                consumed_count += 1;
            }
            consumed_count
        })
        .collect();

    // Wait for all consumers to finish
    let _results = join_all(futures).await;
}

// --- Criterion Benchmarks ---

fn benchmark_group(c: &mut Criterion) {
    let item_counts = [1_000, 10_000, 100_000];

    let rt = Runtime::new().expect("Failed to create Tokio runtime");

    // --- 1. Contention Benchmarks (5 Consumers) ---
    let mut contention_group = c.benchmark_group("Contention (N=5)");

    contention_group.measurement_time(Duration::from_secs(10));
    contention_group.sample_size(50);

    for &count in item_counts.iter() {
        // --- SharedStream (Contended Synchronization) ---
        contention_group.bench_function(&format!("SharedStream_MEM__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_in_memory_stream(count);
                    let shared_stream = SharedStream::new(stream);
                    let consumers: Vec<_> =
                        (0..NUM_CONSUMERS).map(|_| shared_stream.clone()).collect();
                    consume_streams(consumers).await;
                })
            })
        });

        contention_group.bench_function(&format!("SharedStream_IO__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_simulated_io_stream(count);
                    let shared_stream = SharedStream::new(stream);
                    let consumers: Vec<_> =
                        (0..NUM_CONSUMERS).map(|_| shared_stream.clone()).collect();
                    consume_streams(consumers).await;
                })
            })
        });

        // --- Channel Fan-Out (Centralized I/O) ---
        contention_group.bench_function(&format!("ChannelFanOut_MEM__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_in_memory_stream(count);
                    let consumers = run_channel_fan_out(stream);
                    consume_streams(consumers).await;
                })
            })
        });

        contention_group.bench_function(&format!("ChannelFanOut_IO__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_simulated_io_stream(count);
                    let consumers = run_channel_fan_out(stream);
                    consume_streams(consumers).await;
                })
            })
        });
    }
    contention_group.finish();

    // --- 2. Wrapper Overhead Benchmarks (Single Consumer N=1) ---
    let mut overhead_group = c.benchmark_group("Wrapper Overhead (N=1)");
    overhead_group.measurement_time(Duration::from_secs(10));
    overhead_group.sample_size(50);

    for &count in item_counts.iter() {
        // A. Raw Stream Consumption (Baseline, MEM)
        overhead_group.bench_function(&format!("RawStream_MEM__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_in_memory_stream(count);
                    consume_streams(vec![stream]).await;
                })
            })
        });

        // B. Shared Wrapper Consumption (Wrapper Overhead, MEM)
        overhead_group.bench_function(&format!("SharedWrapper_MEM__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_in_memory_stream(count);
                    let shared_stream = SharedStream::new(stream);
                    // Single consumer using the wrapper
                    consume_streams(vec![shared_stream]).await;
                })
            })
        });

        // C. Raw Stream IO Consumption (Baseline, IO)
        overhead_group.bench_function(&format!("RawStream_IO__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_simulated_io_stream(count);
                    consume_streams(vec![stream]).await;
                })
            })
        });

        // D. Shared Wrapper IO Consumption (Wrapper Overhead, IO)
        overhead_group.bench_function(&format!("SharedWrapper_IO__{}", count), |b| {
            b.iter(|| {
                rt.block_on(async {
                    let stream = generate_simulated_io_stream(count);
                    let shared_stream = SharedStream::new(stream);
                    // Single consumer using the wrapper
                    consume_streams(vec![shared_stream]).await;
                })
            })
        });
    }
    overhead_group.finish();
}

criterion_group!(benches, benchmark_group);
criterion_main!(benches);
