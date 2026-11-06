use super::*;

#[test]
fn the_words_are_a_token_count_or_default() {
    assert_eq!(parse("400k"), Some(Some(400_000)));
    assert_eq!(parse(" 1.5M "), Some(Some(1_500_000)));
    assert_eq!(parse("Default"), Some(None));
    assert_eq!(parse("soon"), None);
}

/// 说法照 Puffin `auto_compact::shown` / `saved`（英文逐字相同）。
#[test]
fn the_sentences_are_puffins() {
    assert_eq!(
        shown(Lang::En, None, Some(167_000)),
        "This session compacts at 167k tokens, the model's limit."
    );
    assert_eq!(
        shown(Lang::En, Some(400_000), Some(167_000)),
        "This session compacts at 167k tokens, the model's limit (your setting: 400k)."
    );
    assert_eq!(
        shown(Lang::En, Some(100_000), Some(100_000)),
        "This session compacts at 100k tokens, your setting."
    );
    assert_eq!(
        shown(Lang::En, None, None),
        "Sessions compact at the model's limit."
    );
    assert_eq!(
        saved(Lang::En, Some(400_000)),
        "Saved. Sessions compact at 400k tokens, or at the model's limit if it is lower."
    );
    assert_eq!(shown(Lang::Zh, None, None), "会话在模型的上限压缩。");
}
