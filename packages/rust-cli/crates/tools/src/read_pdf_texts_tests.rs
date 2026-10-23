//! 页码语法与 pdftoppm 报错的分类（tools-files.md §1.5）。

use super::*;

fn range(first: u64, last: Option<u64>) -> PageRange {
    PageRange { first, last }
}

#[test]
fn page_ranges_follow_the_page_syntax() {
    assert_eq!(parse("3"), Some(range(3, Some(3))));
    assert_eq!(parse(" 1-5 "), Some(range(1, Some(5))));
    assert_eq!(parse("2 -\t4"), Some(range(2, Some(4))));
    assert_eq!(parse("10-"), Some(range(10, None)));
    assert_eq!(parse("10 - "), Some(range(10, None)));
    for bad in [
        "",
        "0",
        "0-2",
        "5-2",
        "1,3",
        "1-2-3",
        "a",
        "-3",
        "1 2",
        "1234567890",
        "1-1234567890",
        "１",
    ] {
        assert_eq!(parse(bad), None, "{bad:?}");
    }
    assert!(check("1-20").is_ok());
    assert!(matches!(
        check("1-21"),
        Err(Validation::Invalid {
            error_code: Some(8),
            ..
        })
    ));
    assert!(matches!(
        check("4-"),
        Err(Validation::Invalid {
            error_code: Some(8),
            ..
        })
    ));
    assert!(matches!(
        check("x"),
        Err(Validation::Invalid {
            error_code: Some(7),
            ..
        })
    ));
}

#[test]
fn pdftoppm_errors_are_classified() {
    let one = range(2, Some(3));
    let cases = [
        (
            "Command Line Error: Incorrect password\n",
            "PDF is password-protected. Please provide an unprotected version.",
        ),
        (
            "Wrong page range given: the first page (2) can not be after the last page (0).\n",
            "PDF reports 0 pages (empty page tree). The PDF may be invalid.",
        ),
        (
            "Wrong page range given: the first page (2) can not be after the last page (30).\n",
            "Requested pages 2-3 is outside the document (PDF has 30 pages). Use a range within 1-30, maximum 20 pages per request (e.g. pages: \"1-20\").",
        ),
        (
            "Syntax Error: Couldn't find trailer dictionary\nSyntax Error: Couldn't read xref table\n",
            "PDF file is corrupted or invalid.",
        ),
        (
            "Syntax Error (12): Couldn't read xref table\n",
            "PDF file is corrupted or invalid.",
        ),
        (
            "Internal Error: something damaged\n",
            "PDF file is corrupted or invalid.",
        ),
        (
            "Permission Error: printing this document is not allowed.\n",
            "Could not render PDF: Permission Error: printing this document is not allowed.",
        ),
        (
            "Permission Error: copy not allowed\nInternal Error (3): bad\n",
            "pdftoppm failed: Permission Error: copy not allowed\nInternal Error (3): bad\n",
        ),
        (
            "Could not write image to /tmp/x/page-1.jpg; exiting\n",
            "pdftoppm failed: Could not write image to /tmp/x/page-1.jpg; exiting\n",
        ),
        ("", "pdftoppm failed: "),
    ];
    for (stderr, expected) in cases {
        assert_eq!(render_failure(stderr, one), expected, "{stderr:?}");
    }
    // 范围的写法与单复数。
    let stderr = "Wrong page range given: the first page (5) can not be after the last page (1).";
    assert_eq!(
        render_failure(stderr, range(5, Some(5))),
        "Requested page 5 is outside the document (PDF has 1 page). Use a range within 1-1, maximum 20 pages per request (e.g. pages: \"1-1\")."
    );
}
