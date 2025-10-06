# stream_shared

[![Crates.io](https://img.shields.io/crates/v/stream_shared.svg)](https://crates.io/crates/stream_shared)
[![Documentation](https://docs.rs/stream_shared/badge.svg)](https://docs.rs/stream_shared)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

A Rust library for creating shareable streams that can be cloned and consumed by multiple tasks.

## Overview

`stream_shared` provides `SharedStream`, which allows you to create a stream that can be cloned and shared across multiple consumers. All clones share the same underlying stream state, so clones created at the same time will see the same items, while clones created after partial consumption will only see the remaining items.

## Quick Example

```rust
use stream_shared::SharedStream;
use futures_util::stream;
use futures_util::StreamExt;

#[tokio::main]
async fn main() {
    let data = vec![1, 2, 3, 4, 5];
    let stream = stream::iter(data.clone());
    let shared_stream = SharedStream::new(stream);

    // Clone the stream for multiple consumers
    let consumer1 = shared_stream.clone();
    let consumer2 = shared_stream.clone();

    // Both consumers will receive all items
    let (result1, result2) = tokio::join!(
        consumer1.collect::<Vec<i32>>(),
        consumer2.collect::<Vec<i32>>()
    );

    assert_eq!(result1, data);
    assert_eq!(result2, data);
    println!("Both consumers got: {:?}", result1);
}
```

## Key Features

- **Cloneable streams**: Create multiple consumers from a single stream
- **Thread-safe**: `Send` + `Sync` - clones can be moved across threads
- **Efficient sharing**: Items are cloned only when consumed by each clone
- **Works with any `Unpin` stream**: Compatible with most async streams
- **`!Unpin` support**: Use `Box::pin()` for streams that aren't `Unpin`

## Performance Characteristics

`SharedStream` is optimized for scenarios with multiple consumers. Benchmark results show:

### **Multi-Consumer Performance (5 consumers)**
- **15-50% faster** than channel-based fan-out for small-medium datasets (1K-10K items)
- **More consistent performance** across I/O vs memory-bound workloads
- **Better scaling** with I/O-intensive streams due to reduced context switching

### **Single-Consumer Overhead**
- **3-4x overhead** compared to raw streams for single consumers
- Overhead becomes **relatively smaller** as dataset size increases
- Use raw streams if you only need one consumer

### **When to Use SharedStream**
- ✅ **Multiple consumers** (2+ consumers sharing the same stream)
- ✅ **I/O-bound workloads** (network streams, file streams) - faster at all dataset sizes
- ✅ **Small to medium datasets** with any workload type
- ✅ **Consistent performance requirements**

### **When to Consider Alternatives**
- ❌ **Single consumer only** (use the original stream directly)
- ❌ **Very large pure memory datasets** (100K+ items, channels ~25% faster for memory-only)
- ❌ **Ultra-low latency requirements** (direct consumption is always fastest)

*Benchmarks run on 1KB payloads with 5 consumers. See `benches/` for full benchmark code.*

## Requirements

- The underlying stream must implement `Unpin`
- Stream items must implement `Clone`
- For thread safety, the stream and its items must be `Send` + `Sync`

## License

Licensed under the Apache License, Version 2.0.