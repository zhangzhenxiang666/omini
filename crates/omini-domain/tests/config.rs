use omini_domain::config::{
    InputModality, ModelInfo, ProviderEndpointKind, ProviderInfo, ThinkingEffort,
};
use serde_json::json;

#[test]
fn thinking_effort_all_variants_share_one_string_contract() {
    let cases = [
        (ThinkingEffort::None, "none"),
        (ThinkingEffort::Low, "low"),
        (ThinkingEffort::Medium, "medium"),
        (ThinkingEffort::High, "high"),
        (ThinkingEffort::XHigh, "xhigh"),
        (ThinkingEffort::Max, "max"),
    ];

    assert_eq!(ThinkingEffort::default(), ThinkingEffort::Medium);
    for (effort, text) in cases {
        assert_eq!(effort.to_string(), text);
        assert_eq!(
            serde_json::to_value(effort).expect("thinking effort should serialize"),
            json!(text)
        );
        assert_eq!(
            serde_json::from_value::<ThinkingEffort>(json!(text))
                .expect("thinking effort should deserialize"),
            effort
        );
        assert_eq!(
            text.parse::<ThinkingEffort>(),
            Ok(effort),
            "canonical value should parse"
        );
        assert_eq!(
            format!("  {}  ", text.to_ascii_uppercase()).parse::<ThinkingEffort>(),
            Ok(effort),
            "parsing should ignore case and surrounding whitespace"
        );
    }
}

#[test]
fn thinking_effort_unknown_or_empty_text_is_rejected() {
    for input in ["", "   ", "x-high", "maximum", "unknown"] {
        assert_eq!(
            input.parse::<ThinkingEffort>(),
            Err(()),
            "{input:?} should not be accepted"
        );
    }
}

#[test]
fn modality_and_endpoint_variants_use_lowercase_json_names() {
    for (modality, text) in [
        (InputModality::Text, "text"),
        (InputModality::Image, "image"),
    ] {
        assert_eq!(modality.to_string(), text);
        assert_eq!(
            serde_json::to_value(modality).expect("modality should serialize"),
            json!(text)
        );
    }

    for (endpoint, text) in [
        (ProviderEndpointKind::OpenAI, "openai"),
        (ProviderEndpointKind::Anthropic, "anthropic"),
    ] {
        assert_eq!(
            serde_json::to_value(endpoint).expect("endpoint should serialize"),
            json!(text)
        );
        assert_eq!(
            serde_json::from_value::<ProviderEndpointKind>(json!(text))
                .expect("endpoint should deserialize"),
            endpoint
        );
    }
}

#[test]
fn model_info_missing_optional_fields_default_to_none_and_are_omitted() {
    let input = json!({
        "id": "gpt-test",
        "limit": 256_000,
        "thinking": false
    });

    let model: ModelInfo =
        serde_json::from_value(input.clone()).expect("minimal model should deserialize");
    assert_eq!(model.name, None);
    assert_eq!(model.input_modalities, None);
    assert_eq!(model.extra_headers, None);
    assert_eq!(model.extra_body, None);
    assert_eq!(
        serde_json::to_value(model).expect("minimal model should serialize"),
        input
    );
}

#[test]
fn model_info_present_optional_fields_round_trip_without_losing_nested_values() {
    let input = json!({
        "id": "vision-reasoner",
        "name": "Vision Reasoner",
        "limit": 1_000_000,
        "thinking": true,
        "input_modalities": ["text", "image"],
        "extra_headers": {
            "x-feature": "enabled",
            "x-version": "2026-08-12"
        },
        "extra_body": {
            "boolean": true,
            "number": 42,
            "nested": {"items": [1, "two", null]}
        }
    });

    let model: ModelInfo =
        serde_json::from_value(input.clone()).expect("complete model should deserialize");
    assert_eq!(
        model.input_modalities,
        Some(vec![InputModality::Text, InputModality::Image])
    );
    assert_eq!(
        serde_json::to_value(model).expect("complete model should serialize"),
        input
    );
}

#[test]
fn model_info_missing_required_or_invalid_enum_fields_are_rejected() {
    for input in [
        json!({"id": "missing-limit", "thinking": false}),
        json!({"id": "missing-thinking", "limit": 1}),
        json!({
            "id": "bad-modality",
            "limit": 1,
            "thinking": false,
            "input_modalities": ["audio"]
        }),
    ] {
        assert!(
            serde_json::from_value::<ModelInfo>(input).is_err(),
            "invalid model shape should be rejected"
        );
    }
}

#[test]
fn provider_info_preserves_endpoint_and_model_order() {
    let input = json!({
        "id": "provider",
        "name": "Provider",
        "endpoint": "anthropic",
        "base_url": "https://example.invalid",
        "models": [
            {"id": "first", "limit": 1, "thinking": false},
            {"id": "second", "limit": 2, "thinking": true}
        ]
    });

    let provider: ProviderInfo =
        serde_json::from_value(input.clone()).expect("provider should deserialize");
    assert_eq!(provider.endpoint, ProviderEndpointKind::Anthropic);
    assert_eq!(provider.models[0].id, "first");
    assert_eq!(provider.models[1].id, "second");
    assert_eq!(
        serde_json::to_value(provider).expect("provider should serialize"),
        input
    );
}
