use futures_util::future::{FutureExt, Shared};
use futures_util::stream::{FusedStream, Stream, StreamExt, StreamFuture};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

type SizeHint = (usize, Option<usize>);

/// Internal future wrapper that enables sharing of stream state.
///
/// This type wraps a [`StreamFuture`] and implements the logic for creating
/// shared versions of subsequent futures as the stream is consumed.
#[cfg_attr(test, derive(Debug))]
struct InnerFuture<S>
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
    #[cfg(feature = "stats")]
    stats: crate::stats::Stats,
}

impl<S> Clone for SharedStream<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    fn clone(&self) -> Self {
        let s = Self {
            future: self.future.clone(),
            size_hint: self.size_hint,
            #[cfg(feature = "stats")]
            stats: self.stats.clone(),
        };

        #[cfg(feature = "stats")]
        if self.future.is_some() {
            s.stats.increment();
        }

        s
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

        Self {
            future: InnerFuture::new(stream).shared().into(),
            size_hint,
            #[cfg(feature = "stats")]
            stats: crate::stats::Stats::new(),
        }
    }

    /// Returns the number of active clones of this stream, including the current instance.
    ///
    /// This method allows you to track how many shared consumers exist for the stream.
    /// The count includes the current instance, so a value of 1 means there are no clones.
    ///
    /// # Examples
    ///
    /// ```
    /// use stream_shared::SharedStream;
    /// use futures_util::stream;
    ///
    /// let stream = stream::iter(vec![1, 2, 3]);
    /// let shared = SharedStream::new(stream);
    /// let stats = shared.stats();
    /// assert_eq!(stats.active_clones(), 1); // No clones yet
    ///
    /// let clone1 = shared.clone();
    /// assert_eq!(stats.active_clones(), 2); // Original + 1 clone
    ///
    /// let clone2 = shared.clone();
    /// assert_eq!(stats.active_clones(), 3); // Original + 2 clones
    ///
    /// drop(clone1);
    /// assert_eq!(stats.active_clones(), 2); // Original + 1 clone remaining
    /// ```
    ///
    #[cfg(feature = "stats")]
    #[cfg_attr(docsrs, doc(cfg(feature = "stats")))]
    pub fn stats(&self) -> crate::stats::Stats {
        self.stats.clone()
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
                #[cfg(feature = "stats")]
                {
                    self.stats.decrement();
                }
                Poll::Ready(None)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.size_hint
    }
}

impl<S> FusedStream for SharedStream<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    fn is_terminated(&self) -> bool {
        self.future.is_none()
    }
}

impl<S> Drop for SharedStream<S>
where
    S: Stream + Unpin,
    S::Item: Clone,
{
    fn drop(&mut self) {
        // Decrement active-clone count when this handle is dropped.
        if self.future.is_some() {
            #[cfg(feature = "stats")]
            {
                self.stats.decrement();
            }
        }
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
        use futures_util::stream::FusedStream;
        use futures_util::StreamExt;

        let data = vec![1, 2, 3];
        let stream = stream::iter(data.clone());
        let mut shared_stream = SharedStream::new(stream);

        // Initially, the stream should not be terminated
        assert!(!shared_stream.is_terminated());

        // Consume all items from the stream
        let mut collected = Vec::new();
        while let Some(item) = shared_stream.next().await {
            collected.push(item);
            // While consuming, stream should not be terminated yet
            if collected.len() < data.len() {
                assert!(!shared_stream.is_terminated());
            }
        }
        assert_eq!(collected, data);

        // After exhaustion, the stream should be terminated
        assert!(shared_stream.is_terminated());

        // Now call next() again - should return None
        let result = shared_stream.next().await;
        assert_eq!(result, None);
        assert!(shared_stream.is_terminated());

        // And again to make sure it's consistently None
        let result2 = shared_stream.next().await;
        assert_eq!(result2, None);
        assert!(shared_stream.is_terminated());

        // Test with a cloned stream as well
        let mut cloned_stream = shared_stream.clone();

        // Cloned stream from exhausted stream should also be terminated
        assert!(cloned_stream.is_terminated());

        let result3 = cloned_stream.next().await;
        assert_eq!(result3, None);
        assert!(cloned_stream.is_terminated());
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

    #[tokio::test]
    #[cfg(feature = "stats")]
    async fn test_stats() {
        use futures_util::stream;
        // Tests various `Stats` scenarios: clone, drop, exhaustion,
        // empty-stream handling, and cloning after partial consumption.
        // 1) Creation with non-empty stream
        let stream = stream::iter(vec![1, 2, 3]);
        let shared = SharedStream::new(stream);
        let stats = shared.stats();

        assert_eq!(stats.active_clones(), 1);

        // clone increments
        let clone1 = shared.clone();
        assert_eq!(stats.active_clones(), 2);

        // clone of clone increments
        let clone2 = clone1.clone();
        assert_eq!(stats.active_clones(), 3);

        // dropping a clone decrements
        drop(clone2);
        assert_eq!(stats.active_clones(), 2);

        // exhausting the original handle decrements its contribution
        let _orig_collected: Vec<i32> = shared.collect().await;

        // only clone1 remains
        assert_eq!(stats.active_clones(), 1);

        // dropping the last active clone brings the count to zero
        drop(clone1);
        assert_eq!(stats.active_clones(), 0);

        // empty-stream case: wrapper is counted until polled/exhausted
        let empty_stream = stream::iter(Vec::<i32>::new());
        let shared_empty = SharedStream::new(empty_stream);
        let stats_empty = shared_empty.stats();

        // The stream wrapper exists and hasn't been polled yet, so it's counted.
        assert_eq!(stats_empty.active_clones(), 1);

        let clone_empty = shared_empty.clone();
        assert_eq!(stats_empty.active_clones(), 2);
        drop(clone_empty);
        assert_eq!(stats_empty.active_clones(), 1);

        // exhausting the wrapper clears the count
        let _ = shared_empty.collect::<Vec<i32>>().await;
        assert_eq!(stats_empty.active_clones(), 0);

        // cloning after partial consumption
        let stream2 = stream::iter(vec![10, 20, 30]);
        let mut shared2 = SharedStream::new(stream2);
        let stats2 = shared2.stats();
        assert_eq!(stats2.active_clones(), 1);

        // consume one item
        let first = shared2.next().await;
        assert_eq!(first, Some(10));

        let clone_after = shared2.clone();
        assert_eq!(stats2.active_clones(), 2);

        drop(clone_after);
        assert_eq!(stats2.active_clones(), 1);

        let _ = shared2.collect::<Vec<i32>>().await;
        assert_eq!(stats2.active_clones(), 0);
    }
}
