use super::*;

fn flags(output: OutputFormat, input: InputFormat) -> FormatFlags {
    FormatFlags {
        output,
        input,
        ..FormatFlags::default()
    }
}

/// 取值（choices 之外的报错文字在 args.rs，见 args 的测试）。
#[test]
fn values_map_to_formats() {
    assert_eq!(output_format("stream-json"), Some(OutputFormat::StreamJson));
    assert_eq!(output_format("json"), Some(OutputFormat::Json));
    assert_eq!(output_format("text"), Some(OutputFormat::Text));
    assert_eq!(output_format("xml"), None);
    assert_eq!(input_format("stream-json"), Some(InputFormat::StreamJson));
    assert_eq!(input_format("yaml"), None);
}

#[test]
fn stream_json_input_needs_stream_json_output_and_print() {
    assert_eq!(
        check(flags(OutputFormat::Text, InputFormat::StreamJson), true),
        Err("Error: --input-format=stream-json requires output-format=stream-json.".to_owned())
    );
    assert_eq!(
        check(
            flags(OutputFormat::StreamJson, InputFormat::StreamJson),
            false
        ),
        Err("Error: --input-format=stream-json requires --print.".to_owned())
    );
    assert_eq!(
        check(
            flags(OutputFormat::StreamJson, InputFormat::StreamJson),
            true
        ),
        Ok(())
    );
}

#[test]
fn partial_messages_need_print_and_stream_json() {
    let partial = FormatFlags {
        partial: true,
        ..flags(OutputFormat::Text, InputFormat::Text)
    };
    let error =
        "Error: --include-partial-messages requires --print and --output-format=stream-json.";
    assert_eq!(check(partial, true), Err(error.to_owned()));
    let streaming = FormatFlags {
        output: OutputFormat::StreamJson,
        ..partial
    };
    assert_eq!(check(streaming, false), Err(error.to_owned()));
    assert_eq!(check(streaming, true), Ok(()));
}

/// 空输入的报错排在 `--verbose` 之前；只有空白时另一句。
#[test]
fn input_is_checked_before_verbose() {
    let streaming = flags(OutputFormat::StreamJson, InputFormat::Text);
    assert_eq!(
        check_input(streaming, ""),
        Err("Error: Input must be provided either through stdin or as a prompt argument when using --print".to_owned())
    );
    assert_eq!(
        check_input(streaming, "  "),
        Err("Error: Input contained only whitespace. Provide a prompt with text through stdin or as a prompt argument when using --print".to_owned())
    );
    assert_eq!(
        check_input(streaming, "hi"),
        Err("Error: When using --print, --output-format=stream-json requires --verbose".to_owned())
    );
    let verbose = FormatFlags {
        verbose: true,
        ..streaming
    };
    assert_eq!(check_input(verbose, "hi"), Ok(()));
    // stream-json 输入不看 prompt。
    let input = FormatFlags {
        input: InputFormat::StreamJson,
        ..verbose
    };
    assert_eq!(check_input(input, ""), Ok(()));
    assert_eq!(check_input(FormatFlags::default(), "hi"), Ok(()));
}
