use futures_util::stream::{Stream, StreamExt};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

// --- Data Types and Constants ---

#[derive(Debug)]
pub struct HeavyPayload {
    pub _id: u64,
    pub _payload: [u8; 1024], // 1KB array
}
pub type ArcData = Arc<HeavyPayload>;
pub const NUM_CONSUMERS: usize = 5;

// --- Channel Fan-Out Implementation ---

pub fn run_channel_fan_out<S>(
    mut original_stream: S,
) -> Vec<tokio_stream::wrappers::ReceiverStream<ArcData>>
where
    S: Stream<Item = ArcData> + Unpin + Send + 'static,
{
    let mut txs = Vec::new();
    let mut rx_streams = Vec::new();
    for _ in 0..NUM_CONSUMERS {
        let (tx, rx) = mpsc::channel(1024); // Buffered channel
        txs.push(tx);
        rx_streams.push(ReceiverStream::new(rx));
    }

    tokio::spawn(async move {
        while let Some(item) = original_stream.next().await {
            for tx in &txs {
                let _ = tx.send(item.clone()).await;
            }
        }
    });
    rx_streams
}

// --- Source Stream Generators ---

// Source A: In-Memory (Minimal Read Latency)
pub fn generate_in_memory_stream(
    count: u64,
) -> futures_util::stream::Iter<std::vec::IntoIter<ArcData>> {
    let data: Vec<ArcData> = (0..count)
        .map(|i| {
            Arc::new(HeavyPayload {
                _id: i,
                _payload: [0; 1024],
            })
        })
        .collect();
    futures_util::stream::iter(data)
}

// Source B: Simulated I/O (Introducing Context Switches)
pub struct SimulatedIoStream {
    data: Vec<ArcData>,
    index: usize,
}

impl Stream for SimulatedIoStream {
    type Item = ArcData;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.index >= self.data.len() {
            return Poll::Ready(None);
        }

        // Simulating I/O: Waking up immediately but forcing a poll loop,
        // which introduces context switching and overhead similar to real I/O
        cx.waker().wake_by_ref();

        let item = self.data[self.index].clone();
        self.index += 1;
        Poll::Ready(Some(item))
    }
}

pub fn generate_simulated_io_stream(count: u64) -> impl Stream<Item = ArcData> {
    let data: Vec<ArcData> = (0..count)
        .map(|i| {
            Arc::new(HeavyPayload {
                _id: i,
                _payload: [0; 1024],
            })
        })
        .collect();
    SimulatedIoStream { data, index: 0 }
}
