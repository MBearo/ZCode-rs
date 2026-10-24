//! 内存双工、内存“网络”与字节流上的帧。

use bytes::Bytes;
use tokio::io::AsyncWriteExt;

use crate::link::{Connector, Link, LinkError, Listener};
use crate::memory::{network, pair};
use crate::stream::stream_link;

fn frame(text: &str) -> Bytes {
    Bytes::copy_from_slice(text.as_bytes())
}

#[tokio::test]
async fn a_memory_pair_carries_frames_both_ways_and_reports_closing() {
    let (left, right) = pair("t");
    assert_eq!(left.peer(), "t/server");
    let (mut left_tx, mut left_rx) = Box::new(left).split();
    let (mut right_tx, mut right_rx) = Box::new(right).split();
    left_tx.send(frame("a")).await.unwrap();
    right_tx.send(frame("b")).await.unwrap();
    assert_eq!(right_rx.recv().await.unwrap(), Some(frame("a")));
    assert_eq!(left_rx.recv().await.unwrap(), Some(frame("b")));
    left_tx.close().await.unwrap();
    assert_eq!(right_rx.recv().await.unwrap(), None);
    assert_eq!(left_tx.send(frame("c")).await, Err(LinkError::Closed));
    drop(left_rx);
    assert_eq!(right_tx.send(frame("d")).await, Err(LinkError::Closed));
}

#[tokio::test]
async fn a_memory_network_hands_the_other_end_to_the_listener() {
    let (connector, listener) = network("n");
    assert_eq!(connector.describe(), "memory:n");
    let client = connector.connect().await.unwrap();
    let server = listener.accept().await.unwrap();
    let (mut client_tx, _client_rx) = client.split();
    let (_server_tx, mut server_rx) = server.split();
    client_tx.send(frame("hello")).await.unwrap();
    assert_eq!(server_rx.recv().await.unwrap(), Some(frame("hello")));
    drop(listener);
    assert!(matches!(
        connector.connect().await.map(|_| ()),
        Err(LinkError::Connect(_))
    ));
}

#[tokio::test]
async fn byte_streams_carry_length_prefixed_frames() {
    let (a, b) = tokio::io::duplex(1024);
    let (a_read, a_write) = tokio::io::split(a);
    let (b_read, b_write) = tokio::io::split(b);
    let (mut tx, _) = Box::new(stream_link(a_read, a_write, "a", 16)).split();
    let (_, mut rx) = Box::new(stream_link(b_read, b_write, "b", 16)).split();
    tx.send(frame("one")).await.unwrap();
    tx.send(Bytes::new()).await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), Some(frame("one")));
    assert_eq!(rx.recv().await.unwrap(), Some(Bytes::new()));
    assert_eq!(
        tx.send(Bytes::from(vec![0u8; 17])).await,
        Err(LinkError::TooLarge { size: 17, max: 16 })
    );
    tx.close().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), None);
}

#[tokio::test]
async fn a_stream_ending_inside_a_frame_or_announcing_a_huge_one_is_an_error() {
    let (a, mut b) = tokio::io::duplex(1024);
    let (a_read, a_write) = tokio::io::split(a);
    let (_, mut rx) = Box::new(stream_link(a_read, a_write, "a", 16)).split();
    b.write_all(&[0, 0, 0, 99]).await.unwrap();
    assert_eq!(
        rx.recv().await,
        Err(LinkError::TooLarge { size: 99, max: 16 })
    );
    let (a, mut b) = tokio::io::duplex(1024);
    let (a_read, a_write) = tokio::io::split(a);
    let (_, mut rx) = Box::new(stream_link(a_read, a_write, "a", 16)).split();
    b.write_all(&[0, 0, 0, 5, b'x']).await.unwrap();
    drop(b);
    assert!(matches!(rx.recv().await, Err(LinkError::Io(_))));
    let (a, mut b) = tokio::io::duplex(1024);
    let (a_read, a_write) = tokio::io::split(a);
    let (_, mut rx) = Box::new(stream_link(a_read, a_write, "a", 16)).split();
    b.write_all(&[0, 0]).await.unwrap();
    drop(b);
    assert!(matches!(rx.recv().await, Err(LinkError::Io(_))));
}
