use super::*;

const SHORT: Duration = Duration::from_millis(50);

#[tokio::test]
async fn piped_text_is_read_to_the_end() {
    let piped = read_piped(&b"line one\nline two\n"[..], SHORT)
        .await
        .unwrap();
    assert_eq!(piped, Piped::Text("line one\nline two\n".to_owned()));
    // 立即结束（`< /dev/null`）：空文字，不警告。
    assert_eq!(
        read_piped(&b""[..], SHORT).await.unwrap(),
        Piped::Text(String::new())
    );
}

/// 写入端一直开着、不写：等满时间后放弃（3 秒，写警告的情形）。
#[tokio::test]
async fn a_silent_pipe_is_given_up() {
    let (_writer, reader) = tokio::io::duplex(16);
    assert_eq!(read_piped(reader, SHORT).await.unwrap(), Piped::Silent);
}

/// 有了第一批数据之后一直读到结束，不再限时（收到数据就清掉定时器）。
#[tokio::test]
async fn once_data_arrives_the_rest_is_awaited() {
    use tokio::io::AsyncWriteExt;
    let (mut writer, reader) = tokio::io::duplex(16);
    writer.write_all(b"first ").await.unwrap();
    let write_later = async move {
        tokio::time::sleep(SHORT * 3).await;
        writer.write_all(b"second").await.unwrap();
    };
    let (piped, ()) = tokio::join!(read_piped(reader, SHORT), write_later);
    assert_eq!(piped.unwrap(), Piped::Text("first second".to_owned()));
}

#[tokio::test]
async fn more_than_ten_megabytes_is_refused() {
    let big = vec![b'x'; LIMIT + 1];
    assert_eq!(read_piped(&big[..], SHORT).await.unwrap(), Piped::TooLarge);
    let exact = vec![b'x'; LIMIT];
    assert!(
        matches!(read_piped(&exact[..], SHORT).await.unwrap(), Piped::Text(text) if text.len() == LIMIT)
    );
    assert_eq!(
        too_large(),
        "Error: piped stdin input exceeds 10MB. Pass large content as a file path in your prompt instead."
    );
}

#[test]
fn the_prompt_comes_before_the_piped_text() {
    assert_eq!(
        combine(Some("task".into()), Some("data".into())),
        "task\ndata"
    );
    assert_eq!(combine(None, Some("data".into())), "data");
    assert_eq!(combine(Some(String::new()), Some(String::new())), "");
    assert_eq!(combine(Some("task".into()), None), "task");
}
