//! Extension trait for converting streams into `SharedStream`.
//!
//! This module provides the `SharedStreamExt` trait which adds an `into_shared` method
//! to any type that implements `Stream + Unpin` with clonable items.

use crate::SharedStream;
use futures_util::stream::Stream;

/// Extension trait for [`Stream`] that provides the `into_shared` method.
///
/// This trait allows any stream that meets the requirements to be easily converted
/// into a [`SharedStream`] for sharing across multiple consumers.
pub trait SharedStreamExt: Stream {
    /// Converts this stream into a [`SharedStream`].
    ///
    /// This method consumes the original stream and returns a [`SharedStream`] that
    /// can be cloned to create multiple consumers sharing the same underlying stream state.
    ///
    /// # Requirements
    ///
    /// The stream must satisfy:
    /// - [`Stream`]: The type must implement the Stream trait
    /// - [`Unpin`]: Required for safe polling without additional pinning
    /// - [`Clone`] for `Self::Item`: Stream items must be cloneable
    ///
    /// ```
    /// use stream_shared::{SharedStream, SharedStreamExt};
    /// use futures_util::stream;
    ///
    /// let data = vec!["hello", "world"];
    /// let stream = stream::iter(data);
    /// let shared_stream = stream.into_shared();
    /// ```
    fn into_shared(self) -> SharedStream<Self>
    where
        Self: Sized + Unpin,
        Self::Item: Clone,
    {
        SharedStream::new(self)
    }
}

impl<S> SharedStreamExt for S where S: Stream {}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use futures_util::StreamExt as FuturesStreamExt;

    #[tokio::test]
    async fn test_into_shared_trait_works() {
        let data = vec![1, 2, 3];
        let stream = stream::iter(data.clone());

        let shared_stream: SharedStream<_> = stream.into_shared();
        let result: Vec<i32> = shared_stream.collect().await;

        assert_eq!(result, data);
    }
}
