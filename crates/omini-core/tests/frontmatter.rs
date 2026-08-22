use omini_core::frontmatter;
use serde_yaml::Value;

#[test]
fn frontmatter_bom_and_crlf_parse_mapping_and_body() {
    let (mapping, body) = frontmatter::parse(
        "\u{feff}---\r\ntags: [one, two]\r\nmeta:\r\n  enabled: true\r\n---\r\nbody\r\n",
    )
    .expect("frontmatter should parse");

    assert_eq!(body, "body\r\n");
    assert_eq!(
        frontmatter::optional_string_list(&mapping, "tags"),
        Ok(Some(vec!["one".into(), "two".into()]))
    );
    assert_eq!(
        frontmatter::optional_bool_path(&mapping, &["meta", "enabled"]),
        Ok(Some(true))
    );
}

#[test]
fn frontmatter_missing_or_invalid_required_values_are_rejected() {
    for content in ["body", "---\nname: test\n"] {
        let error = frontmatter::parse(content).expect_err("invalid delimiters should reject");
        assert!(error.starts_with("missing "), "unexpected error: {error}");
    }

    let mapping =
        frontmatter::parse_yaml("name: '   '\nlist: [one, 2]").expect("yaml mapping should parse");
    assert_eq!(
        frontmatter::required_string(&mapping, "name"),
        Err("frontmatter field 'name' must not be empty".into())
    );
    assert_eq!(
        frontmatter::optional_string_list(&mapping, "list"),
        Err("frontmatter field 'list' must be a string".into())
    );
}

#[test]
fn frontmatter_null_yaml_becomes_empty_mapping() {
    let mapping = frontmatter::parse_yaml("~").expect("null frontmatter should be accepted");
    assert!(mapping.is_empty());
    assert_eq!(frontmatter::get(&mapping, "missing"), None);
    assert_eq!(
        frontmatter::get_path(&mapping, &["missing", "nested"]),
        None
    );
    assert_ne!(Value::Null, Value::Bool(false));
}
