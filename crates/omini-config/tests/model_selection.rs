use omini_config::{ModelTier, ModelTierEntry, ModelTiers, Settings, UserConfig};
use omini_domain::config::{InputModality, ProviderEndpointKind, ThinkingEffort};

fn config_from_toml(input: &str) -> UserConfig {
    toml::from_str(input).expect("config fixture should parse")
}

fn multi_provider_config(tiers: &str) -> UserConfig {
    config_from_toml(&format!(
        r#"
[providers.openai]
name = "OpenAI"
endpoint = "openai"
base_url = "https://openai.example"
api_key = "openai-key"

[providers.openai.models]
fast = {{}}
reasoner = {{ thinking = true, input_modalities = ["text", "image"] }}

[providers.anthropic]
name = "Anthropic"
endpoint = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models]
haiku = {{}}
opus = {{ thinking = true }}

{tiers}
"#
    ))
}

fn settings_with_tiers(
    active_provider: &str,
    active_model: &str,
    active_effort: Option<ThinkingEffort>,
    model_tiers: ModelTiers,
) -> Settings {
    let mut settings = multi_provider_config("")
        .to_settings(Some(active_provider), Some(active_model), active_effort)
        .expect("settings should build");
    settings.model_tiers = model_tiers;
    settings
}

fn tier_entry(
    provider: &str,
    model: &str,
    thinking_effort: Option<ThinkingEffort>,
) -> ModelTierEntry {
    ModelTierEntry {
        provider: provider.into(),
        model: model.into(),
        thinking_effort,
    }
}

#[test]
fn model_tiers_cover_all_slots() {
    let tiers = ModelTiers {
        small: Some(tier_entry("p-small", "m-small", None)),
        standard: Some(tier_entry("p-standard", "m-standard", None)),
        large: Some(tier_entry("p-large", "m-large", None)),
    };
    let cases = [
        (ModelTier::Small, "small", "p-small", "m-small"),
        (ModelTier::Standard, "standard", "p-standard", "m-standard"),
        (ModelTier::Large, "large", "p-large", "m-large"),
    ];

    assert_eq!(ModelTier::ALL, cases.map(|case| case.0));
    for (tier, label, provider, model) in cases {
        assert_eq!(tier.as_str(), label);
        assert_eq!(
            tiers.get(tier),
            Some(&ModelTierEntry {
                provider: provider.into(),
                model: model.into(),
                thinking_effort: None,
            })
        );
    }
}

#[test]
fn model_conversion_preserves_capabilities() {
    let config = config_from_toml(
        r#"
language = "  简体中文  "

[providers.openai]
name = "OpenAI Compatible"
endpoint = "openai"
base_url = "https://openai.example/v1"
api_key = "secret"

[providers.openai.models.plain]

[providers.openai.models."vision.reasoner"]
name = "Vision Reasoner"
limit = 1000000
thinking = true
input_modalities = ["text", "image"]

[providers.openai.models."vision.reasoner".headers]
x-feature = "enabled"

[providers.openai.models."vision.reasoner".body]
routing = "quality"
nested = { count = 2 }
"#,
    );

    let settings = config
        .to_settings(
            Some("openai"),
            Some("vision.reasoner"),
            Some(ThinkingEffort::XHigh),
        )
        .expect("settings should build");

    assert_eq!(settings.active_provider, "openai");
    assert_eq!(settings.model, "vision.reasoner");
    assert_eq!(settings.endpoint, ProviderEndpointKind::OpenAI);
    assert_eq!(settings.base_url, "https://openai.example/v1");
    assert_eq!(settings.api_key, "secret");
    assert_eq!(settings.language.as_deref(), Some("  简体中文  "));
    assert_eq!(settings.thinking_effort, Some(ThinkingEffort::XHigh));
    assert!(settings.compact.enabled);
    assert_eq!(settings.compact.preserve_recent, 6);
    assert_eq!(settings.compact.buffer_tokens, 13_000);
    assert_eq!(settings.compact.summary_output_tokens, 20_000);
    assert_eq!(settings.compact.max_consecutive_failures, 3);

    let provider = &settings.providers["openai"];
    assert_eq!(provider.name, "OpenAI Compatible");
    let plain = provider
        .models
        .iter()
        .find(|model| model.id == "plain")
        .unwrap();
    assert_eq!(plain.name, None);
    assert_eq!(plain.limit, 256_000);
    assert!(!plain.thinking);
    assert_eq!(plain.input_modalities, None);
    assert_eq!(plain.extra_headers, None);
    assert_eq!(plain.extra_body, None);

    let selected = settings
        .current_model_config()
        .expect("selected model should exist");
    assert_eq!(selected.name.as_deref(), Some("Vision Reasoner"));
    assert_eq!(selected.limit, 1_000_000);
    assert!(selected.thinking);
    assert_eq!(
        selected.input_modalities.as_deref(),
        Some([InputModality::Text, InputModality::Image].as_slice())
    );
    assert_eq!(
        selected.extra_headers.as_ref().unwrap(),
        &std::collections::HashMap::from([("x-feature".into(), "enabled".into())])
    );
    assert_eq!(
        selected.extra_body.as_ref().unwrap(),
        &serde_json::Map::from_iter([
            ("routing".into(), serde_json::json!("quality")),
            ("nested".into(), serde_json::json!({"count": 2})),
        ])
    );
}

#[test]
fn single_candidate_is_the_selection_fallback() {
    let config = config_from_toml(
        r#"
[providers.only]
endpoint = "anthropic"
base_url = "https://only.example"
api_key = "key"

[providers.only.models.sole]
thinking = true
"#,
    );
    let cases = [
        (None, None),
        (Some("missing-provider"), Some("missing-model")),
        (Some("only"), Some("missing-model")),
    ];

    for (provider, model) in cases {
        let settings = config
            .to_settings(provider, model, None)
            .expect("single candidate should be selected");
        assert_eq!(settings.active_provider, "only");
        assert_eq!(settings.model, "sole");
        assert_eq!(settings.thinking_effort, Some(ThinkingEffort::Medium));
    }
}

#[test]
fn model_capabilities_normalize_effort_and_modalities() {
    let config = multi_provider_config("");
    let cases = [
        ("fast", Some(ThinkingEffort::High), None, false, false),
        ("reasoner", None, Some(ThinkingEffort::Medium), true, true),
        (
            "reasoner",
            Some(ThinkingEffort::Max),
            Some(ThinkingEffort::Max),
            true,
            true,
        ),
    ];

    for (model, requested, expected, thinking, image) in cases {
        let settings = config
            .to_settings(Some("openai"), Some(model), requested)
            .expect("settings should build");
        assert_eq!(settings.thinking_effort, expected);
        assert_eq!(settings.current_model_supports_thinking(), thinking);
        assert_eq!(
            settings.supports_input_modality(InputModality::Image),
            image
        );
        assert_eq!(settings.supports_input_modality(InputModality::Text), image);
        assert!(!settings.model_supports_thinking("missing", model));
        assert!(!settings.model_supports_thinking("openai", "missing"));
    }
}

#[test]
fn tier_schema_defaults_and_parses_slots() {
    let omitted = multi_provider_config("");
    assert_eq!(omitted.model_tiers, ModelTiers::default());

    let configured = multi_provider_config(
        r#"
[model_tiers.small]
provider = "anthropic"
model = "haiku"
thinking_effort = "low"

[model_tiers.standard]
provider = "openai"
model = "fast"

[model_tiers.large]
provider = "anthropic"
model = "opus"
thinking_effort = "high"
"#,
    );

    assert_eq!(
        configured.model_tiers,
        ModelTiers {
            small: Some(tier_entry("anthropic", "haiku", Some(ThinkingEffort::Low))),
            standard: Some(tier_entry("openai", "fast", None)),
            large: Some(tier_entry("anthropic", "opus", Some(ThinkingEffort::High))),
        }
    );
}

#[test]
fn invalid_tier_schema_is_rejected() {
    let cases = [
        (
            r#"
[model_tiers.unknown]
provider = "openai"
model = "fast"
"#,
            "unknown",
        ),
        (
            r#"
[model_tiers.small]
provider = "openai"
model = "fast"
extra = true
"#,
            "extra",
        ),
        (
            r#"
[model_tiers.small]
provider = "openai"
model = "fast"
thinking_effort = "extreme"
"#,
            "extreme",
        ),
    ];

    for (invalid_tiers, reason) in cases {
        let input = format!(
            r#"
[providers.openai]
endpoint = "openai"
base_url = "https://example.invalid"
api_key = "key"

[providers.openai.models.fast]

{invalid_tiers}
"#
        );
        let error = toml::from_str::<UserConfig>(&input)
            .expect_err("invalid tier schema should be rejected");
        let message = error.to_string();
        assert!(
            message.contains(reason),
            "error should identify {reason:?}: {message}"
        );
    }
}

#[test]
fn valid_tiers_resolve_across_providers() {
    let settings = settings_with_tiers(
        "openai",
        "fast",
        None,
        ModelTiers {
            small: Some(tier_entry("openai", "reasoner", Some(ThinkingEffort::Low))),
            standard: Some(tier_entry("anthropic", "haiku", Some(ThinkingEffort::High))),
            large: Some(tier_entry("anthropic", "opus", None)),
        },
    );

    let cases = [
        (
            ModelTier::Small,
            (
                "openai".to_string(),
                "reasoner".to_string(),
                Some(ThinkingEffort::Low),
            ),
        ),
        (
            ModelTier::Standard,
            ("anthropic".to_string(), "haiku".to_string(), None),
        ),
        (
            ModelTier::Large,
            (
                "anthropic".to_string(),
                "opus".to_string(),
                Some(ThinkingEffort::Medium),
            ),
        ),
    ];

    for (tier, expected) in cases {
        assert_eq!(settings.resolve_tier(tier), expected);
    }
}

#[test]
fn invalid_tiers_fall_back_to_active_model() {
    let fallback = (
        "openai".to_string(),
        "reasoner".to_string(),
        Some(ThinkingEffort::High),
    );
    let cases = [
        (ModelTier::Small, ModelTiers::default()),
        (
            ModelTier::Standard,
            ModelTiers {
                standard: Some(tier_entry("missing", "fast", None)),
                ..Default::default()
            },
        ),
        (
            ModelTier::Large,
            ModelTiers {
                large: Some(tier_entry("openai", "missing", None)),
                ..Default::default()
            },
        ),
    ];

    for (tier, tiers) in cases {
        let settings = settings_with_tiers("openai", "reasoner", Some(ThinkingEffort::High), tiers);
        assert_eq!(settings.resolve_tier(tier), fallback);
    }
}

#[test]
fn tier_resolution_is_read_only() {
    let settings = settings_with_tiers(
        "openai",
        "reasoner",
        Some(ThinkingEffort::Low),
        ModelTiers {
            small: Some(tier_entry("anthropic", "opus", Some(ThinkingEffort::High))),
            standard: Some(tier_entry("openai", "fast", None)),
            large: None,
        },
    );
    let original_tiers = settings.model_tiers.clone();

    for _ in 0..3 {
        for tier in ModelTier::ALL {
            let _ = settings.resolve_tier(tier);
        }
    }

    assert_eq!(settings.model_tiers, original_tiers);
    assert_eq!(settings.active_provider, "openai");
    assert_eq!(settings.model, "reasoner");
    assert_eq!(settings.thinking_effort, Some(ThinkingEffort::Low));
}
