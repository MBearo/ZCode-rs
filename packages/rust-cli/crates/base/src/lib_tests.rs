// 复制自 rust/crates/async/src/lib_tests.rs（feat/rs 79b6691c55）。
use super::*;

#[tokio::test]
async fn returns_the_output_when_the_future_completes() {
    assert_eq!(catch_panic(async { 7 }).await, Ok(7));
}

#[tokio::test]
async fn turns_string_panics_into_errors() {
    let message = String::from("boom");
    let result = catch_panic(async move { panic!("{message}") }).await;
    assert_eq!(
        result,
        Err(Panicked {
            message: "boom".to_owned()
        })
    );
}

#[tokio::test]
async fn turns_static_str_and_other_payloads_into_errors() {
    let result: Result<(), _> = catch_panic(async { panic!("static") }).await;
    assert_eq!(
        result.map_err(|error| error.message),
        Err("static".to_owned())
    );
    let result: Result<(), _> = catch_panic(async { std::panic::panic_any(42_u8) }).await;
    assert_eq!(
        result.map_err(|error| error.to_string()),
        Err("task panicked: non-string panic payload".to_owned())
    );
}

/// 修复线 F109：本线程已经记着一条 panic 文字时不覆盖，交回那一条（展开中又 panic）；被 base 捕获的 panic 清掉
/// 本线程的记录，同一线程下一次 panic 不会被当成展开中的第二次。
#[tokio::test]
async fn a_caught_panic_clears_the_note_of_its_thread() {
    assert_eq!(note_panic("first".to_owned()), None);
    assert_eq!(note_panic("second".to_owned()), Some("first".to_owned()));
    assert_eq!(take_panic_note(), None);
    let caught: Result<(), _> = catch_panic(async {
        assert_eq!(note_panic("caught".to_owned()), None);
        panic!("boom")
    })
    .await;
    assert!(caught.is_err());
    assert_eq!(take_panic_note(), None);
    // 由句柄拥有的任务：在工作线程上捕获，清的是那个线程的记录。
    let task = owned(async {
        note_panic("owned".to_owned());
        if true {
            panic!("boom");
        }
        1
    });
    assert!(task.await.is_err());
    assert_eq!(owned_note().await, None);
}

/// 同一个（单线程）运行时上的下一个任务读到的记录。
async fn owned_note() -> Option<String> {
    owned(async { take_panic_note() }).await.unwrap()
}
