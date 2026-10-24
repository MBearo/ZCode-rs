//! 故障注入：丢帧、扣住一帧放到下一帧之后、延迟、发几帧后切断、随时切断。

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::fault::{Faults, inject};
use crate::link::{Link, LinkError};
use crate::memory::pair;

fn frame(text: &str) -> Bytes {
    Bytes::copy_from_slice(text.as_bytes())
}

fn starts_with(prefix: &'static str) -> crate::fault::Matcher {
    Arc::new(move |bytes: &[u8]| bytes.starts_with(prefix.as_bytes()))
}

#[tokio::test]
async fn matching_frames_are_dropped_a_limited_number_of_times() {
    let (left, right) = pair("t");
    let (left, _switch) = inject(Box::new(left), Faults::new().drop_sent(starts_with("x"), 1));
    let (mut tx, _rx) = left.split();
    let (_peer_tx, mut peer_rx) = Box::new(right).split();
    for text in ["x1", "a", "x2"] {
        tx.send(frame(text)).await.unwrap();
    }
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("a")));
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("x2")));
}

#[tokio::test]
async fn a_held_frame_is_released_after_the_next_one() {
    let (left, right) = pair("t");
    let (left, _switch) = inject(Box::new(left), Faults::new().hold_sent(starts_with("ack")));
    let (mut tx, _rx) = left.split();
    let (_peer_tx, mut peer_rx) = Box::new(right).split();
    for text in ["ack 5", "ack 9", "ack 12"] {
        tx.send(frame(text)).await.unwrap();
    }
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("ack 9")));
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("ack 5")));
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("ack 12")));
}

#[tokio::test]
async fn frames_can_be_delayed() {
    let (left, right) = pair("t");
    let (left, _switch) = inject(
        Box::new(left),
        Faults::new().delay_sent(Duration::from_millis(30)),
    );
    let (mut tx, _rx) = left.split();
    let (_peer_tx, mut peer_rx) = Box::new(right).split();
    let started = Instant::now();
    tx.send(frame("late")).await.unwrap();
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("late")));
    assert!(started.elapsed() >= Duration::from_millis(30));
}

#[tokio::test]
async fn the_link_is_cut_after_a_number_of_frames() {
    let (left, right) = pair("t");
    let (left, switch) = inject(Box::new(left), Faults::new().cut_after_sent(2));
    let (mut tx, mut rx) = left.split();
    let (mut peer_tx, mut peer_rx) = Box::new(right).split();
    tx.send(frame("1")).await.unwrap();
    tx.send(frame("2")).await.unwrap();
    assert!(switch.is_cut());
    assert_eq!(tx.send(frame("3")).await, Err(LinkError::Closed));
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("1")));
    assert_eq!(peer_rx.recv().await.unwrap(), Some(frame("2")));
    assert_eq!(peer_rx.recv().await.unwrap(), None);
    assert_eq!(rx.recv().await, Err(LinkError::Closed));
    assert_eq!(peer_tx.send(frame("back")).await, Err(LinkError::Closed));
}

#[tokio::test]
async fn the_switch_cuts_an_idle_link_in_both_directions() {
    let (left, right) = pair("t");
    let (left, switch) = inject(Box::new(left), Faults::new());
    let (_tx, mut rx) = left.split();
    let (_peer_tx, mut peer_rx) = Box::new(right).split();
    let waiting = rust_cli_base::owned(async move { rx.recv().await });
    tokio::task::yield_now().await;
    switch.cut();
    assert_eq!(waiting.await.unwrap(), Err(LinkError::Closed));
    assert_eq!(peer_rx.recv().await.unwrap(), None);
}
