use super::*;
use crate::config::ProviderType;
use crate::image_transform_cache::ImageTransformCache;
use crate::urp::{AnthropicCacheTarget, UrpRequest};
use serde_json::json;

const TRANSFORMS: [&str; 3] = [
    "cache_anthropic_system",
    "cache_anthropic_tool_use",
    "cache_anthropic_auto",
];

async fn context(path: &std::path::Path) -> TransformRuntimeContext {
    let _ = rustls::crypto::ring::default_provider().install_default();
    TransformRuntimeContext {
        image_transform_cache: Arc::new(
            ImageTransformCache::new(path.to_path_buf(), std::time::Duration::from_secs(60))
                .await
                .unwrap(),
        ),
        http_client: reqwest::Client::new(),
        upstream_provider_type: Some(ProviderType::Messages),
    }
}

fn request() -> UrpRequest {
    serde_json::from_value(json!({
        "model": "claude-sonnet-4",
        "input": [
            {"type": "text", "role": "system", "content": "system"},
            {"type": "text", "role": "user", "content": "lookup"},
            {"type": "tool_call", "call_id": "call_1", "name": "lookup", "arguments": "{}"},
            {"type": "tool_result", "call_id": "call_1", "content": [{"type": "text", "text": "found"}]}
        ]
    })).unwrap()
}

fn target<'a>(req: &'a mut UrpRequest, transform: &str) -> &'a mut HashMap<String, Value> {
    match transform {
        "cache_anthropic_system" => req.input[0].extra_body_mut(),
        "cache_anthropic_tool_use" => req.input[3].extra_body_mut(),
        "cache_anthropic_auto" => &mut req.extra_body,
        _ => unreachable!(),
    }
}

async fn apply(
    req: &mut UrpRequest,
    transform: &str,
    config: Value,
    context: &TransformRuntimeContext,
) {
    let rules = [TransformRuleConfig {
        transform: transform.into(),
        enabled: true,
        models: None,
        phase: Phase::Request,
        config,
    }];
    let registry = registry();
    let mut states = build_states_for_rules(&rules, &registry).unwrap();
    apply_transforms(
        UrpData::Request(req),
        &rules,
        &mut states,
        "claude-sonnet-4",
        Phase::Request,
        context,
        &registry,
    )
    .await
    .unwrap();
}

#[test]
fn cache_ttl_config_rejects_invalid_values_and_unknown_fields() {
    let registry = registry();
    for transform in TRANSFORMS {
        let transform = registry.get(transform).unwrap();
        for config in [json!({}), json!({"ttl": "5m"}), json!({"ttl": "1h"})] {
            assert!(transform.parse_config(config).is_ok());
        }
        for config in [
            json!({"ttl": "2h"}),
            json!({"ttl": null}),
            json!({"ttl": 5}),
            json!({"unknown": true}),
        ] {
            assert!(transform.parse_config(config).is_err());
        }
    }
}

#[tokio::test]
async fn later_rules_override_owned_ttl_in_both_directions() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    for transform in TRANSFORMS {
        let mut req = request();
        apply(&mut req, transform, json!({}), &context).await;
        assert_eq!(
            target(&mut req, transform)["cache_control"],
            json!({"type": "ephemeral"})
        );
        apply(&mut req, transform, json!({"ttl": "1h"}), &context).await;
        assert_eq!(
            target(&mut req, transform)["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
        apply(&mut req, transform, json!({"ttl": "5m"}), &context).await;
        assert_eq!(
            target(&mut req, transform)["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(req.context.anthropic_cache_markers.len(), 1);
        let wire = crate::urp::encode::anthropic::encode_request(&req, "upstream");
        assert!(!wire.to_string().contains("anthropic_cache_markers"));
    }
}

#[tokio::test]
async fn cache_ttl_never_overrides_client_markers_or_trusted_context() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    for transform in TRANSFORMS {
        let mut req = request();
        let client = json!({"type": "ephemeral", "ttl": "1h", "client_extension": true});
        target(&mut req, transform).insert("cache_control".into(), client.clone());
        let mut wire = serde_json::to_value(&req).unwrap();
        wire["context"] =
            json!({"anthropic_cache_markers": ["System", "LastToolResult", "Request"]});
        let mut req: UrpRequest = serde_json::from_value(wire).unwrap();
        apply(&mut req, transform, json!({"ttl": "5m"}), &context).await;
        assert_eq!(target(&mut req, transform)["cache_control"], client);
        assert!(req.context.anthropic_cache_markers.is_empty());
        assert!(
            !serde_json::to_value(&req)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("context")
        );
    }
}

#[tokio::test]
async fn owned_cache_marker_can_retune_at_four_slots_without_adding_another_slot() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    for transform in TRANSFORMS {
        let mut req = request();
        apply(&mut req, transform, json!({}), &context).await;
        let mut added = 0;
        for node in &mut req.input {
            let extra = node.extra_body_mut();
            if !extra.contains_key("cache_control") && added < 3 {
                extra.insert("cache_control".into(), json!({"type": "ephemeral"}));
                added += 1;
            }
        }
        assert_eq!(added, 3);
        apply(&mut req, transform, json!({"ttl": "1h"}), &context).await;
        assert_eq!(target(&mut req, transform)["cache_control"]["ttl"], "1h");
        assert_eq!(req.context.anthropic_cache_markers.len(), 1);
        let slots = req
            .input
            .iter_mut()
            .map(|node| usize::from(node.extra_body_mut().contains_key("cache_control")))
            .sum::<usize>()
            + usize::from(req.extra_body.contains_key("cache_control"));
        assert_eq!(slots, 4);
    }
}

#[tokio::test]
async fn new_attempts_start_without_previous_attempt_marker_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    let original = request();
    let mut first = original.clone();
    apply(
        &mut first,
        "cache_anthropic_system",
        json!({"ttl": "1h"}),
        &context,
    )
    .await;
    assert!(
        first
            .context
            .anthropic_cache_markers
            .contains_key(&AnthropicCacheTarget::System)
    );
    let mut retry = original.clone();
    assert!(retry.context.anthropic_cache_markers.is_empty());
    apply(&mut retry, "cache_anthropic_system", json!({}), &context).await;
    assert_eq!(
        retry.input[0].extra_body_mut()["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[tokio::test]
async fn deleting_an_owned_system_node_does_not_transfer_ownership_to_a_client_marker() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    let mut req = request();
    let client = json!({"type": "ephemeral"});
    req.input[0]
        .extra_body_mut()
        .insert("cache_control".into(), client.clone());
    req.input.insert(
        1,
        serde_json::from_value(json!({
            "type": "text", "role": "system", "content": "x-anthropic-billing-header: transient"
        }))
        .unwrap(),
    );
    apply(&mut req, "cache_anthropic_system", json!({}), &context).await;
    apply(
        &mut req,
        "prompt_strip_anthropic_billing_header",
        json!({}),
        &context,
    )
    .await;
    apply(
        &mut req,
        "cache_anthropic_system",
        json!({"ttl": "1h"}),
        &context,
    )
    .await;
    assert_eq!(req.input.len(), 4);
    assert_eq!(req.input[0].extra_body_mut()["cache_control"], client);
    assert!(req.context.anthropic_cache_markers.is_empty());
}

#[tokio::test]
async fn reordered_system_nodes_do_not_transfer_ownership_to_a_client_marker() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    let mut req = request();
    let client = json!({"type": "ephemeral"});
    req.input[0]
        .extra_body_mut()
        .insert("cache_control".into(), client.clone());
    req.input.insert(
        1,
        serde_json::from_value(json!({
            "type": "text", "role": "system", "content": "later system"
        }))
        .unwrap(),
    );
    apply(&mut req, "cache_anthropic_system", json!({}), &context).await;
    req.input.swap(0, 1);
    apply(
        &mut req,
        "cache_anthropic_system",
        json!({"ttl": "1h"}),
        &context,
    )
    .await;
    assert_eq!(req.input[1].extra_body_mut()["cache_control"], client);
    assert!(req.context.anthropic_cache_markers.is_empty());
}

#[tokio::test]
async fn replaced_marker_values_are_not_treated_as_monoize_owned() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    for transform in TRANSFORMS {
        let mut req = request();
        apply(&mut req, transform, json!({}), &context).await;
        let replacement = json!({"type": "ephemeral", "ttl": "1h", "custom": true});
        target(&mut req, transform).insert("cache_control".into(), replacement.clone());
        apply(&mut req, transform, json!({"ttl": "5m"}), &context).await;
        assert_eq!(target(&mut req, transform)["cache_control"], replacement);
        assert!(req.context.anthropic_cache_markers.is_empty());
    }
}

#[tokio::test]
async fn indistinguishable_nodes_never_transfer_marker_ownership_after_reordering() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(temp.path()).await;
    for duplicate_after_insertion in [false, true] {
        let mut req = request();
        let client = json!({"type": "ephemeral"});
        req.input[0]
            .extra_body_mut()
            .insert("cache_control".into(), client.clone());
        req.input.insert(
            1,
            serde_json::from_value(json!({
                "type": "text", "role": "system",
                "content": if duplicate_after_insertion { "different" } else { "system" }
            }))
            .unwrap(),
        );
        apply(&mut req, "cache_anthropic_system", json!({}), &context).await;
        assert_eq!(
            req.context.anthropic_cache_markers.is_empty(),
            !duplicate_after_insertion
        );
        if duplicate_after_insertion {
            let Node::Text { content, .. } = &mut req.input[0] else {
                panic!("system text")
            };
            *content = "different".into();
        }
        assert_eq!(req.input[0], req.input[1]);
        req.input.swap(0, 1);
        apply(
            &mut req,
            "cache_anthropic_system",
            json!({"ttl": "1h"}),
            &context,
        )
        .await;
        assert_eq!(req.input[0].extra_body_mut()["cache_control"], client);
        assert_eq!(req.input[1].extra_body_mut()["cache_control"], client);
        assert!(req.context.anthropic_cache_markers.is_empty());
    }
}
