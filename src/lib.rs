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
//! - **Memory overhead**: Each item must be cloned for every active consumer
//! - **Synchronization cost**: Uses `Shared<Future>` internally, which has coordination overhead
//! - **Item lifetime**: Items are kept in memory until all clones have consumed them
//!
//! For best performance:
//! - Minimize the number of concurrent clones when possible
//! - Prefer small, cheap-to-clone items (consider `Arc<T>` for large data)
//!

mod ext;

pub use ext::SharedStreamExt;

use futures_util::future::{FutureExt, Shared};
use futures_util::stream::{Stream, StreamExt, StreamFuture};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

type SizeHint = (usize, Option<usize>);

/// Internal future wrapper that enables sharing of stream state.
///
/// This type wraps a [`StreamFuture`] and implements the logic for creating
/// shared versions of subsequent futures as the stream is consumed.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct InnerFuture<S>
where
    S: Stream + Unpin,
{
    inner: Option<StreamFuture<S>>,
}

impl<S> InnerFuture<S>
where
    S: Stream + Unpin,
{
    /// Creates a new `InnerFuture` from the given stream.
    pub(crate) fn new(stream: S) -> Self {
        InnerFuture {
            inner: Some(stream.into_future()),
        }
    }
}

impl<S> Future for InnerFuture<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    // The output type is changed to reflect the attempt to return a shared future.
    type Output = Option<(S::Item, Shared<Self>, SizeHint)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner_future = match self.inner.as_mut() {
            Some(f) => Pin::new(f),
            None => return Poll::Ready(None),
        };

        match inner_future.poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready((Some(item), stream)) => {
                let size_hint = stream.size_hint();
                let next_shared_future = InnerFuture::new(stream).shared();
                self.inner.take();
                Poll::Ready(Some((item, next_shared_future, size_hint)))
            }
            Poll::Ready((None, _stream)) => {
                self.inner.take();
                Poll::Ready(None)
            }
        }
    }
}
/// A cloneable stream wrapper that allows multiple consumers to share the same stream.
///
/// `SharedStream` wraps any [`Stream`] that is [`Unpin`] and whose items implement [`Clone`],
/// allowing it to be cloned and consumed by multiple tasks simultaneously. All clones
/// share the same underlying stream state and position.
///
/// # Examples
///
/// Basic usage with concurrent consumers:
///
/// ```
/// use stream_shared::SharedStream;
/// use futures_util::stream;
/// use futures_util::StreamExt;
///
/// # tokio_test::block_on(async {
/// let data = vec!["hello", "world", "from", "rust"];
/// let stream = stream::iter(data.clone());
/// let shared_stream = SharedStream::new(stream);
///
/// // Create multiple consumers
/// let consumer1 = shared_stream.clone();
/// let consumer2 = shared_stream.clone();
///
/// // Both will receive all items
/// let (result1, result2) = tokio::join!(
///     consumer1.collect::<Vec<&str>>(),
///     consumer2.collect::<Vec<&str>>()
/// );
///
/// assert_eq!(result1, data);
/// assert_eq!(result2, data);
/// # });
/// ```
///
/// Cloning after partial consumption:
///
/// ```
/// use stream_shared::SharedStream;
/// use futures_util::stream;
/// use futures_util::StreamExt;
///
/// # tokio_test::block_on(async {
/// let data = vec![1, 2, 3, 4, 5];
/// let stream = stream::iter(data);
/// let mut shared_stream = SharedStream::new(stream);
///
/// // Consume first item
/// let first = shared_stream.next().await;
/// assert_eq!(first, Some(1));
///
/// // Clone after partial consumption
/// let cloned = shared_stream.clone();
/// let remaining: Vec<i32> = cloned.collect().await;
///
/// // Clone only sees remaining items
/// assert_eq!(remaining, vec![2, 3, 4, 5]);
/// # });
/// ```
///
/// # Requirements
///
/// The wrapped stream must satisfy these bounds:
/// - [`Stream`]: The type must implement the Stream trait
/// - [`Unpin`]: Required for safe polling without pinning
/// - [`Stream::Item`]: The item should be clonable
///
/// For [`!Unpin`](Unpin) streams, pin them first:
///
/// ```
/// use stream_shared::SharedStream;
/// use futures_util::stream::{Stream, StreamExt};
/// use std::pin::Pin;
/// use std::task::{Context, Poll};
///
/// // Create a custom !Unpin stream
/// #[derive(Clone)]
/// struct NotUnpinStream {
///     data: Vec<i32>,
///     index: usize,
///     _pin: std::marker::PhantomPinned,
/// }
///
/// impl Stream for NotUnpinStream {
///     type Item = i32;
///     
///     fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
///         // SAFETY: We only modify fields that are not structurally pinned
///         let this = unsafe { self.get_unchecked_mut() };
///         if this.index < this.data.len() {
///             let item = this.data[this.index];
///             this.index += 1;
///             Poll::Ready(Some(item))
///         } else {
///             Poll::Ready(None)
///         }
///     }
/// }
///
/// # tokio_test::block_on(async {
/// let not_unpin_stream = NotUnpinStream {
///     data: vec![1, 2, 3],
///     index: 0,
///     _pin: std::marker::PhantomPinned,
/// };
///
/// // This wouldn't compile: SharedStream::new(not_unpin_stream)
/// // But this works:
/// let pinned_stream = Box::pin(not_unpin_stream);
/// let shared = SharedStream::new(pinned_stream);
/// let result: Vec<i32> = shared.collect().await;
/// assert_eq!(result, vec![1, 2, 3]);
/// # });
/// ```
#[derive(Debug)]
pub struct SharedStream<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    // The current shared future which holds the state of the stream.
    // We use a Shared<InnerFuture<S>> to manage the sharing.
    future: Option<Shared<InnerFuture<S>>>,
    size_hint: SizeHint,
}

impl<S> Clone for SharedStream<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    fn clone(&self) -> Self {
        Self {
            future: self.future.clone(),
            size_hint: self.size_hint,
        }
    }
}

impl<S> SharedStream<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    /// Creates a new `SharedStream` from the given stream.
    ///
    /// The stream must implement [`Unpin`] and its items must implement [`Clone`].
    /// Once created, the `SharedStream` can be cloned to create multiple consumers
    /// that all share the same underlying stream state.
    ///
    /// # Examples
    ///
    /// ```
    /// use stream_shared::SharedStream;
    /// use futures_util::stream;
    ///
    /// let data = vec![1, 2, 3, 4, 5];
    /// let stream = stream::iter(data);
    /// let shared_stream = SharedStream::new(stream);
    /// ```
    ///
    /// # Panics
    ///
    /// This method does not panic under normal circumstances.
    pub fn new(stream: S) -> Self {
        let size_hint = stream.size_hint();
        SharedStream {
            future: InnerFuture::new(stream).shared().into(),
            size_hint,
        }
    }
}

impl<S> Stream for SharedStream<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let poll_result = match &mut self.future {
            Some(f) => Pin::new(f).poll(cx),
            None => return Poll::Ready(None),
        };

        match poll_result {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some((item, next_shared_future, size_hint))) => {
                // Replace the future with the new shared version.
                self.future = next_shared_future.into();
                self.size_hint = size_hint;
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                self.future.take();
                Poll::Ready(None)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.size_hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    #[tokio::test]
    async fn test_basic_shared_stream_works() {
        let original_data = vec![1, 2, 3, 4, 5];
        let stream = stream::iter(original_data.clone());

        let shared_stream = SharedStream::new(stream);

        assert_eq!(shared_stream.size_hint(), (5, Some(5)));

        let collected: Vec<i32> = shared_stream.collect().await;
        assert_eq!(collected, original_data);
    }

    #[tokio::test]
    async fn test_multiple_clones_get_same_data() {
        // This is the main test - each clone should see all the original items
        let data = vec![10, 20, 30];
        let stream = stream::iter(data.clone());

        let shared_stream = SharedStream::new(stream);
        let clone1 = shared_stream.clone();
        let clone2 = shared_stream.clone();
        let clone3 = shared_stream.clone();

        assert_eq!(clone1.size_hint(), (3, Some(3)));
        assert_eq!(clone2.size_hint(), (3, Some(3)));
        assert_eq!(clone3.size_hint(), (3, Some(3)));

        // Run all clones at the same time to make sure they don't interfere
        let (result1, result2, result3) = tokio::join!(
            clone1.collect::<Vec<i32>>(),
            clone2.collect::<Vec<i32>>(),
            clone3.collect::<Vec<i32>>()
        );

        // Every clone should have gotten the complete original data
        assert_eq!(result1, data);
        assert_eq!(result2, data);
        assert_eq!(result3, data);
    }

    #[tokio::test]
    async fn test_clone_after_partial_consumption() {
        use futures_util::StreamExt;

        let numbers = vec![100, 200, 300, 400];
        let stream = stream::iter(numbers.clone());
        let mut shared_stream = SharedStream::new(stream);

        // Initial size hint should be 4
        assert_eq!(shared_stream.size_hint(), (4, Some(4)));

        // Take one item first
        let first_item = shared_stream.next().await;
        assert_eq!(first_item, Some(100));

        assert_eq!(shared_stream.size_hint(), (3, Some(3)));

        // Now make a clone and see what it gets
        let cloned_stream = shared_stream.clone();

        assert_eq!(cloned_stream.size_hint(), (3, Some(3)));

        let clone_result: Vec<i32> = cloned_stream.collect().await;

        // The clone gets what's left from the current position, not everything
        assert_eq!(clone_result, vec![200, 300, 400]);
    }

    #[tokio::test]
    async fn test_with_string_data() {
        // Make sure it works with other Clone types, not just numbers
        let messages = vec!["hello".to_string(), "world".to_string()];
        let stream = stream::iter(messages.clone());
        let shared_stream = SharedStream::new(stream);

        let clone1 = shared_stream.clone();
        let clone2 = shared_stream.clone();

        let (result1, result2) = tokio::join!(
            clone1.collect::<Vec<String>>(),
            clone2.collect::<Vec<String>>()
        );

        assert_eq!(result1, messages);
        assert_eq!(result2, messages);
    }

    #[tokio::test]
    async fn test_empty_stream_behavior() {
        // Edge case: what happens with empty streams?
        let empty_stream = stream::iter(Vec::<i32>::new());
        let shared_stream = SharedStream::new(empty_stream);

        assert_eq!(shared_stream.size_hint(), (0, Some(0)));

        let clone1 = shared_stream.clone();
        let clone2 = shared_stream.clone();

        assert_eq!(clone1.size_hint(), (0, Some(0)));
        assert_eq!(clone2.size_hint(), (0, Some(0)));

        let (result1, result2) =
            tokio::join!(clone1.collect::<Vec<i32>>(), clone2.collect::<Vec<i32>>());

        assert!(result1.is_empty());
        assert!(result2.is_empty());
    }

    #[tokio::test]
    async fn test_single_item_stream() {
        use futures_util::StreamExt;

        // Another edge case: streams with just one item
        let single_item = vec![42];
        let stream = stream::iter(single_item.clone());
        let mut shared_stream = SharedStream::new(stream);

        assert_eq!(shared_stream.size_hint(), (1, Some(1)));

        let clone1 = shared_stream.clone();
        let clone2 = shared_stream.clone();

        assert_eq!(clone1.size_hint(), (1, Some(1)));
        assert_eq!(clone2.size_hint(), (1, Some(1)));

        let (result1, result2) =
            tokio::join!(clone1.collect::<Vec<i32>>(), clone2.collect::<Vec<i32>>());

        assert_eq!(result1, single_item);
        assert_eq!(result2, single_item);

        let item = shared_stream.next().await;
        assert_eq!(item, Some(42));

        assert_eq!(shared_stream.size_hint(), (0, Some(0)));

        // Verify stream is exhausted
        let remaining: Vec<i32> = shared_stream.collect().await;
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn test_many_clones_stress_test() {
        // Stress test with lots of clones to make sure nothing breaks
        let data = vec![1, 2, 3];
        let stream = stream::iter(data.clone());
        let shared_stream = SharedStream::new(stream);

        // Make 20 clones - this should still work fine
        let mut clone_futures = Vec::new();
        for _ in 0..20 {
            let clone = shared_stream.clone();
            clone_futures.push(clone.collect::<Vec<i32>>());
        }

        let all_results = futures_util::future::join_all(clone_futures).await;

        // Every single clone should have the complete data
        for result in all_results {
            assert_eq!(result, data);
        }
    }

    #[tokio::test]
    async fn test_not_unpin_stream_with_box_pin() {
        use std::marker::PhantomPinned;
        use std::task::Context;

        // Create a custom !Unpin stream to test Box::pin() works
        #[derive(Clone)]
        struct NotUnpinStream {
            data: Vec<i32>,
            index: usize,
            _pin: PhantomPinned,
        }

        impl Stream for NotUnpinStream {
            type Item = i32;

            fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                // SAFETY: We only modify fields that are not structurally pinned
                let this = unsafe { self.get_unchecked_mut() };
                if this.index < this.data.len() {
                    let item = this.data[this.index];
                    this.index += 1;
                    Poll::Ready(Some(item))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let not_unpin_stream = NotUnpinStream {
            data: vec![10, 20, 30],
            index: 0,
            _pin: PhantomPinned,
        };

        // Compile-time verification that NotUnpinStream is actually !Unpin
        static_assertions::assert_not_impl_any!(NotUnpinStream: Unpin);

        // This stream is !Unpin, so we need to pin it
        let pinned_stream = Box::pin(not_unpin_stream);
        let shared_stream = SharedStream::new(pinned_stream);

        // Test that it works with clones
        let clone1 = shared_stream.clone();
        let clone2 = shared_stream.clone();

        let (result1, result2) =
            tokio::join!(clone1.collect::<Vec<i32>>(), clone2.collect::<Vec<i32>>());

        assert_eq!(result1, vec![10, 20, 30]);
        assert_eq!(result2, vec![10, 20, 30]);
    }

    #[test]
    fn test_send_sync_bounds() {
        // Compile-time verification that SharedStream is Send + Sync
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        type TestStream = SharedStream<futures_util::stream::Iter<std::vec::IntoIter<i32>>>;

        assert_send::<TestStream>();
        assert_sync::<TestStream>();

        // Also verify using static_assertions
        static_assertions::assert_impl_all!(TestStream: Send, Sync);
    }

    #[tokio::test]
    async fn test_cross_thread_sharing() {
        use std::sync::Arc;
        use tokio::task;

        let data = vec![1, 2, 3, 4, 5];
        let stream = stream::iter(data.clone());
        let shared_stream = Arc::new(SharedStream::new(stream));

        // Clone and move to different threads
        let stream1 = Arc::clone(&shared_stream);
        let stream2 = Arc::clone(&shared_stream);

        let handle1 = task::spawn(async move {
            let cloned_stream = (*stream1).clone();
            cloned_stream.collect::<Vec<i32>>().await
        });

        let handle2 = task::spawn(async move {
            let cloned_stream = (*stream2).clone();
            cloned_stream.collect::<Vec<i32>>().await
        });

        let (result1, result2) = tokio::join!(handle1, handle2);
        assert_eq!(result1.unwrap(), data);
        assert_eq!(result2.unwrap(), data);
    }

    #[tokio::test]
    async fn test_next_after_stream_exhausted() {
        use futures_util::StreamExt;

        let data = vec![1, 2, 3];
        let stream = stream::iter(data.clone());
        let mut shared_stream = SharedStream::new(stream);

        // Consume all items from the stream
        let mut collected = Vec::new();
        while let Some(item) = shared_stream.next().await {
            collected.push(item);
        }
        assert_eq!(collected, data);

        // Now call next() again - should return None
        let result = shared_stream.next().await;
        assert_eq!(result, None);

        // And again to make sure it's consistently None
        let result2 = shared_stream.next().await;
        assert_eq!(result2, None);

        // Test with a cloned stream as well
        let mut cloned_stream = shared_stream.clone();
        let result3 = cloned_stream.next().await;
        assert_eq!(result3, None);
    }

    #[tokio::test]
    async fn test_pending_future_behavior() {
        use futures_util::StreamExt;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};
        use std::task::{Context, Poll, Waker};

        // Create a custom stream that returns Pending once, then Ready
        #[derive(Clone)]
        struct PendingOnceStream {
            data: Vec<i32>,
            index: usize,
            has_returned_pending: Arc<Mutex<bool>>,
            stored_waker: Arc<Mutex<Option<Waker>>>,
        }

        impl Stream for PendingOnceStream {
            type Item = i32;

            fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                let this = self.get_mut();
                let mut has_returned_pending = this.has_returned_pending.lock().unwrap();

                // Return Pending exactly once to test the Pending code path
                if !*has_returned_pending {
                    *has_returned_pending = true;
                    *this.stored_waker.lock().unwrap() = Some(cx.waker().clone());

                    // Immediately wake the waker to continue execution
                    let waker = cx.waker().clone();
                    waker.wake();

                    return Poll::Pending;
                }

                // After returning Pending once, behave normally
                if this.index < this.data.len() {
                    let item = this.data[this.index];
                    this.index += 1;
                    Poll::Ready(Some(item))
                } else {
                    Poll::Ready(None)
                }
            }
        }

        let has_returned_pending = Arc::new(Mutex::new(false));
        let stored_waker = Arc::new(Mutex::new(None));

        let pending_stream = PendingOnceStream {
            data: vec![100, 200],
            index: 0,
            has_returned_pending: Arc::clone(&has_returned_pending),
            stored_waker: Arc::clone(&stored_waker),
        };

        let shared_stream = SharedStream::new(pending_stream);

        // This should succeed and exercise the Pending code path
        let result = shared_stream.collect::<Vec<i32>>().await;
        assert_eq!(result, vec![100, 200]);

        // Verify that Pending was actually returned once
        assert!(*has_returned_pending.lock().unwrap());
    }

    #[tokio::test]
    async fn test_inner_future_direct_poll() {
        use futures_util::stream;
        use std::future::Future;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        // Test InnerFuture directly to hit the edge case where inner is None
        let stream = stream::iter(vec![42]);
        let mut inner_future = InnerFuture::new(stream);

        // Create a dummy waker for polling
        let waker = futures_util::task::noop_waker();
        let mut cx = Context::from_waker(&waker);

        // First poll should return the item
        let result = Pin::new(&mut inner_future).poll(&mut cx);
        assert!(matches!(result, Poll::Ready(Some((42, _, _)))));

        // Poll again - this should hit the None case and return Poll::Ready(None)
        let result = Pin::new(&mut inner_future).poll(&mut cx);
        assert!(matches!(result, Poll::Ready(None)));
    }
}
