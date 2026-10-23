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
