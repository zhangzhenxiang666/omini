use omini_core::{TitleGenError, title_generation::parse_generated_title};

#[test]
fn generated_title_json_and_fences_return_trimmed_title() {
    let cases = [
        (r#"{"title":"Fix login bug"}"#, "Fix login bug"),
        (
            "```json\n{\"title\": \"修复登录 bug\"}\n```",
            "修复登录 bug",
        ),
        (
            "```\n{\"title\": \"  Review flaky test  \"}\n```",
            "Review flaky test",
        ),
    ];

    for (raw, expected) in cases {
        assert_eq!(parse_generated_title(raw), Ok(expected.into()));
    }
}

#[test]
fn generated_title_invalid_shape_or_json_returns_parse_error() {
    for raw in [
        r#"{"description":"missing"}"#,
        r#"{"title":"   "}"#,
        "not json",
    ] {
        assert!(matches!(
            parse_generated_title(raw),
            Err(TitleGenError::Parse(_))
        ));
    }
}
