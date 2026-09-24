use super::*;

fn image(name: &str, payload: &str) -> Value {
    json!({"type": "image", "mediaType": "image/png",
        "dataUrl": format!("data:image/png;base64,{payload}"),
        "source": {"placeholder": name}})
}

fn properties(video: bool) -> Value {
    json!({"inputFormat": {"supportsImage": true, "supportsVideo": video}})
}

#[tokio::test]
async fn history_media_over_the_budget_become_omission_text() {
    // 每个 data URL 长 22 + 8 = 30 字节；预算 70：最新输入的 30 受保护，历史从新到旧只放得下一个。
    let mut messages = vec![
        json!({"role": "system", "content": "s"}),
        json!({"role": "user", "content": [image("old.png", "AAAAAAAA")]}),
        json!({"role": "user", "content": [image("mid.png", "BBBBBBBB")]}),
        json!({"role": "user", "content": [{"type": "text", "text": "latest"}, image("new.png", "CCCCCCCC")]}),
    ];
    assert!(
        materialize_within(&mut messages, &properties(true), 70)
            .await
            .unwrap()
    );
    assert_eq!(
        messages[1]["content"][0]["text"],
        format!("[Attached image/png: old.png]\n{}", media_budget::OMITTED)
    );
    assert_eq!(messages[2]["content"][0]["type"], "image_url");
    assert_eq!(
        messages[3]["content"][1]["image_url"]["url"],
        "data:image/png;base64,CCCCCCCC"
    );
}

#[tokio::test]
async fn unsupported_media_do_not_count_and_current_media_can_fail() {
    let video = json!({"type": "video", "mediaType": "video/mp4",
        "dataUrl": format!("data:video/mp4;base64,{}", "A".repeat(400)), "source": {"placeholder": "v.mp4"}});
    let mut messages = vec![
        json!({"role": "user", "content": [video]}),
        json!({"role": "user", "content": [image("new.png", "CCCCCCCC")]}),
    ];
    // 不支持的视频先换成说明文本，不占预算。
    materialize_within(&mut messages, &properties(false), 30)
        .await
        .unwrap();
    assert!(
        messages[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("does not support video input")
    );
    assert_eq!(messages[1]["content"][0]["type"], "image_url");
    let mut current = vec![json!({"role": "user", "content": [image("new.png", "CCCCCCCC")]})];
    let failure = materialize_within(&mut current, &properties(true), 29)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "MEDIA_BUDGET_CURRENT_ATTACHMENT_TOO_LARGE");
}
