use std::time::Duration;

use futures_util::stream;

use super::*;

#[tokio::test]
async fn items_arrive_then_an_ended_stream_waits_forever() {
    let mut watched = Watched::new(stream::iter([1, 2]).boxed());
    assert_eq!(watched.next().await, 1);
    assert_eq!(watched.next().await, 2);
    let ended = tokio::time::timeout(Duration::from_millis(20), watched.next()).await;
    assert!(ended.is_err());
}

#[tokio::test]
async fn a_stopped_stream_is_not_polled_again() {
    let mut watched = Watched::new(stream::iter([1]).boxed());
    watched.stop();
    let stopped = tokio::time::timeout(Duration::from_millis(20), watched.next()).await;
    assert!(stopped.is_err());
}
