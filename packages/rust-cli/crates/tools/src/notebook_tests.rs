use super::*;

#[test]
fn cells_render_with_ids_types_and_outputs() {
    let parsed = parse(
        r##"{"cells":[{"cell_type":"code","id":"c1","source":["a\n","b"],"execution_count":3,
        "outputs":[{"output_type":"stream","text":["1\n"]}]},{"cell_type":"markdown","source":"# T"}],
        "metadata":{"language_info":{"name":"python"}},"nbformat":4,"nbformat_minor":5}"##,
    )
    .unwrap();
    let blocks = render(&cells(&parsed));
    let [ToolResultContent::Text { text }] = blocks.as_slice() else {
        panic!("{blocks:?}");
    };
    assert_eq!(
        text,
        "<cell id=\"c1\">a\nb</cell id=\"c1\">\n\n1\n\n<cell id=\"cell-1\"><cell_type>markdown</cell_type># T</cell id=\"cell-1\">"
    );
}

#[test]
fn invalid_notebooks_are_rejected() {
    assert_eq!(parse("{").unwrap_err(), texts::NOTEBOOK_BAD_JSON);
    assert_eq!(
        parse(r#"{"cells":[{"source":1}]}"#).unwrap_err(),
        texts::NOTEBOOK_INVALID
    );
    assert_eq!(index_from_id("cell-12"), Some(12));
    assert_eq!(index_from_id("cell-x"), None);
}

#[test]
fn serialization_uses_one_space_indent() {
    let value: Value = serde_json::from_str(r#"{"cells":[],"a":{"b":1}}"#).unwrap();
    assert_eq!(
        serialize(&value),
        "{\n \"cells\": [],\n \"a\": {\n  \"b\": 1\n }\n}"
    );
}
