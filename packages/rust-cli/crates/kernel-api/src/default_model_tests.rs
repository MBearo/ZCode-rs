//! 缺省模型的晚到与定下（performance.md §12.3）。
use super::*;

fn selection(model: &str) -> ModelSelection {
    ModelSelection::new("p", model)
}

#[tokio::test]
async fn a_known_selection_is_settled_at_once() {
    let default = DefaultModel::from(selection("m"));
    assert!(default.is_settled());
    assert_eq!(default.current(), selection("m"));
    assert_eq!(default.settled().await, selection("m"));
}

#[tokio::test]
async fn a_pending_default_is_empty_until_it_is_settled() {
    let (settle, default) = DefaultModel::pending();
    let copy = default.clone();
    assert!(!default.is_settled());
    assert_eq!(default.current(), unselected());
    let waiting = tokio::spawn(async move { copy.settled().await });
    settle.settle(selection("late"));
    assert_eq!(waiting.await.unwrap(), selection("late"));
    assert_eq!(default.current(), selection("late"));
}

#[tokio::test]
async fn waiting_ends_with_no_model_when_nobody_settles() {
    let (settle, default) = DefaultModel::pending();
    drop(settle);
    assert_eq!(default.settled().await, unselected());
}

/// performance.md §12.4：暂定值只供显示，定下之前照样等。
#[tokio::test]
async fn a_guess_shows_until_the_default_settles() {
    let (settle, default) = DefaultModel::guessed(selection("last time"));
    assert!(!default.is_settled());
    assert_eq!(default.current(), selection("last time"));
    let copy = default.clone();
    let waiting = tokio::spawn(async move { copy.settled().await });
    settle.settle(selection("now"));
    assert_eq!(waiting.await.unwrap(), selection("now"));
    assert_eq!(default.current(), selection("now"));
}
