use super::*;

#[test]
fn usage_adds_every_field() {
    let mut total = Usage {
        input_tokens: 1,
        output_tokens: 2,
        cache_creation_input_tokens: 3,
        cache_read_input_tokens: 4,
    };
    total += Usage {
        input_tokens: 10,
        output_tokens: 20,
        cache_creation_input_tokens: 30,
        cache_read_input_tokens: 40,
    };
    assert_eq!(
        total,
        Usage {
            input_tokens: 11,
            output_tokens: 22,
            cache_creation_input_tokens: 33,
            cache_read_input_tokens: 44,
        }
    );
}

#[test]
fn the_text_of_a_result_joins_the_text_blocks() {
    let content = vec![
        ToolResultContent::Text { text: "a".into() },
        ToolResultContent::ToolReference {
            tool_name: "Read".into(),
        },
        ToolResultContent::Text { text: "b".into() },
    ];
    assert_eq!(text_of(&content), "a\nb");
    assert_eq!(text_of(&[]), "");
}
