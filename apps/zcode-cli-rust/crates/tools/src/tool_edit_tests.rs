//! Node `handlers/edit.ts` failure texts, replacement and result content.
use super::*;

fn code_of(error: anyhow::Error) -> (u32, String) {
    match error.downcast::<crate::contract::ToolError>().unwrap() {
        crate::contract::ToolError::Handler { code, message } => (code, message),
        other => panic!("{other:?}"),
    }
}

#[test]
fn deleting_text_also_deletes_its_newline() {
    let planned = plan("a\nb\nc\n", "b", "", false, "b").unwrap();
    assert_eq!(planned.content, "a\nc\n");
    let planned = plan("a b\n", "b\n", "", false, "b\n").unwrap();
    assert_eq!(planned.content, "a ");
}

#[test]
fn failures_quote_the_original_old_string() {
    let (code, message) = code_of(plan("abc", "zz", "y", false, "zz\r\n").err().unwrap());
    assert_eq!(code, code::OLD_STRING_NOT_FOUND);
    assert_eq!(
        message,
        "String to replace not found in file.\nString: zz\r\n"
    );
    let (code, message) = code_of(plan("x x x", "x", "y", false, "x").err().unwrap());
    assert_eq!(code, code::AMBIGUOUS_REPLACE);
    assert!(
        message.starts_with("Found 3 matches of the string to replace, but replace_all is false.")
    );
    assert!(message.ends_with("\nString: x"));
    assert_eq!(plan("x x x", "x", "y", true, "x").unwrap().content, "y y y");
}

#[test]
fn fallbacks_keep_the_file_quote_style() {
    let planned = plan("say “hi”\n", "say \"hi\"", "say \"bye\"", false, "").unwrap();
    assert_eq!(planned.strategy, "quote_normalized");
    assert_eq!(planned.content, "say “bye”\n");
    let planned = plan("a\tb", "a\\tb", "c\\td", false, "").unwrap();
    assert_eq!(planned.content, "c\td");
}

#[test]
fn line_endings_follow_the_majority_and_content_names_the_given_path() {
    assert!(crlf("a\r\nb\r\nc\n"));
    assert!(!crlf("a\r\nb\nc\n"));
    assert!(!crlf(""));
    assert_eq!(
        model_content("src/a.rs", false),
        "The file src/a.rs has been updated successfully. (file state is current in your context — no need to Read it back)"
    );
    assert_eq!(
        model_content("a", true),
        "The file a has been updated. All occurrences were successfully replaced. (file state is current in your context — no need to Read it back)"
    );
}

#[tokio::test]
async fn missing_files_suggest_a_similar_name() {
    let dir = std::env::temp_dir().join(format!("zcode-edit-{}", crate::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("config.json"), "{}")
        .await
        .unwrap();
    tokio::fs::write(dir.join("main.rs"), "").await.unwrap();
    let message = missing_message(&dir.join("config.yaml"), Path::new("/w")).await;
    assert_eq!(
        message,
        "File does not exist. Note: your current working directory is /w. Did you mean config.json?"
    );
    let message = missing_message(&dir.join("mian.rs"), Path::new("/w")).await;
    assert!(message.ends_with("Did you mean main.rs?"), "{message}");
    let message = missing_message(&dir.join("zzzzzzzz.txt"), Path::new("/w")).await;
    assert!(!message.contains("Did you mean"));
    tokio::fs::remove_dir_all(&dir).await.unwrap();
}
