//! A library for creating shareable streams that can be cloned and consumed by multiple tasks.
//!
//! [`SharedStream`] wraps any [`Stream`] to make it cloneable. All clones share the same underlying
//! stream state, so clones created at the same time will see the same items, while clones created
//! after partial consumption will only see the remaining items.
//!
//! # Examples
//!
//! ```
//! use stream_shared::SharedStream;
//! use futures_util::stream;
//! use futures_util::StreamExt;
//!
//! # tokio_test::block_on(async {
//! let data = vec![1, 2, 3, 4, 5];
//! let stream = stream::iter(data.clone());
//! let shared_stream = SharedStream::new(stream);
//!
//! // Clone the stream for multiple consumers
//! let consumer1 = shared_stream.clone();
//! let consumer2 = shared_stream.clone();
//!
//! // Both consumers will receive all items
//! let result1: Vec<i32> = consumer1.collect().await;
//! let result2: Vec<i32> = consumer2.collect().await;
//!
//! assert_eq!(result1, data);
//! assert_eq!(result2, data);
//! # });
//! ```
//!
//! # Requirements
//!
//! The underlying [`Stream`] type must be [`Unpin`] and the stream's items must implement [`Clone`].
//! With a [`!Unpin`](Unpin) stream, you'll first have to pin the stream. This can
//! be done by boxing the stream using [`Box::pin`] or pinning it to the stack using the `pin_mut!`
//! macro from the `pin_utils` crate.
//!
//! # Behavior
//!
//! When you clone a [`SharedStream`], the clone will start from the current position
//! of the stream being cloned, not from the beginning of the original data. Each
//! `SharedStream` maintains its own independent position. This means:
//!
//! - Clones created from the same stream at the same time will see the same items
//! - Clones created after consumption will only see items remaining from that stream's position
//! - Each clone can be consumed independently and can itself be cloned from its current position
//!
//! For example, with a stream containing 20 items:
//! ```
//! use stream_shared::SharedStream;
//! use futures_util::stream;
//!
//! let data = (1..=20).collect::<Vec<i32>>();
//! let stream_with_20_items = stream::iter(data);
//! let original = SharedStream::new(stream_with_20_items);
//! // ... consume 10 items from original ...
//! let clone1 = original.clone();  // clone1 will have 10 remaining items
//!
//! // ... consume 2 items from clone1 ...  
//! let clone2 = clone1.clone();    // clone2 will have 8 remaining items
//!
//! // Each stream maintains its own position independently
//! let clone3 = original.clone();  // clone3 will have 10 remaining items
//! ```
//!
//! # Thread Safety
//!
//! `SharedStream` is both [`Send`] and [`Sync`] when the underlying stream and its items
//! are `Send` and `Sync`. This means cloned streams can be safely moved across threads
//! and shared between tasks running on different threads.
//!
//! ```
//! use stream_shared::SharedStream;
//! use futures_util::stream;
//! use futures_util::StreamExt;
//! use std::sync::Arc;
//! use tokio::task;
//!
//! # tokio_test::block_on(async {
//! let data = vec![1, 2, 3, 4, 5];
//! let stream = stream::iter(data.clone());
//! let shared_stream = SharedStream::new(stream);
//!
//! // Clone and move to different threads
//! let stream1 = shared_stream.clone();
//! let stream2 = shared_stream.clone();
//!
//! let handle1 = task::spawn(async move {
//!     stream1.collect::<Vec<i32>>().await
//! });
//!
//! let handle2 = task::spawn(async move {
//!     stream2.collect::<Vec<i32>>().await
//! });
//!
//! let (result1, result2) = tokio::join!(handle1, handle2);
//! assert_eq!(result1.unwrap(), data);
//! assert_eq!(result2.unwrap(), data);
//! # });
//! ```
//!
//! # Performance Considerations
//!
//! `SharedStream` introduces some overhead compared to consuming a stream directly:
//!
//! - **Memory overhead**: Each item must be cloned for every active consumer (only when consumed)
//! - **Synchronization cost**: Uses `Shared<Future>` internally, which has coordination overhead
//! - **Item lifetime**: Items are kept in memory until all clones have consumed them
//!
//! For best performance:
//! - Prefer cheap-to-clone small items or `Arc<T>` for large data
//!

#![cfg_attr(docsrs, feature(doc_cfg))]

mod ext;
mod shared_stream;

pub use ext::SharedStreamExt;
pub use shared_stream::SharedStream;

#[cfg_attr(docsrs, doc(cfg(feature = "stats")))]
#[cfg(feature = "stats")]
pub mod stats;

#[cfg(doc)]
use futures_util::Stream;
