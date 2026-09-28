use super::*;
use crate::config::ProviderType;
use crate::image_transform_cache::ImageTransformCache;
use crate::urp::{ToolResultContent, UrpRequest};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;

async fn context(provider: ProviderType, path: &std::path::Path) -> TransformRuntimeContext {
    let _ = rustls::crypto::ring::default_provider().install_default();
    TransformRuntimeContext {
        image_transform_cache: Arc::new(
            ImageTransformCache::new(path.to_path_buf(), std::time::Duration::from_secs(60))
                .await
                .unwrap(),
        ),
        http_client: reqwest::Client::new(),
        upstream_provider_type: Some(provider),
    }
}

async fn apply_request(
    id: &str,
    req: &mut UrpRequest,
    config: Value,
    context: &TransformRuntimeContext,
) {
    let registry = registry();
    let transform = registry.get(id).expect("registered transform");
    let config = transform.parse_config(config).unwrap();
    transform
        .apply(
            UrpData::Request(req),
            Phase::Request,
            context,
            config.as_ref(),
            transform.init_state().as_mut(),
        )
        .await
        .unwrap();
}

fn request(input: Value) -> UrpRequest {
    serde_json::from_value(json!({"model":"claude-sonnet-4", "input": input})).unwrap()
}

fn annotated_text() -> Value {
    json!({"type":"text", "role":"assistant", "content":"hello", "id":"text-id",
        "signature":"signed-original", "logprobs":[{"token":"hello", "logprob":-0.1}],
        "citations":[{"origin_protocol":"responses", "source":{"kind":"url", "url":"https://example.com"}}]})
}

#[tokio::test]
async fn merging_roles_preserves_annotated_text_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Responses, temp.path()).await;
    let mut req = request(json!([annotated_text(), {"type":"text", "role":"assistant", "content":"more"}]));
    let original = req.input.clone();
    apply_request("role_merge_consecutive", &mut req, json!({}), &context).await;
    assert_eq!(req.input, original);
}

#[tokio::test]
async fn noop_think_extraction_preserves_text_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Responses, temp.path()).await;
    let original = request(json!([annotated_text()])).input;
    let registry = registry();
    let transform = registry.get("reasoning_from_think_xml").unwrap();
    let config = transform.parse_config(json!({"tag":"think"})).unwrap();
    let mut response: crate::urp::UrpResponse = serde_json::from_value(json!({"id":"test-response", "model":"test", "output":original})).unwrap();
    transform.apply(UrpData::Response(&mut response), Phase::Response, &context,
        config.as_ref(), transform.init_state().as_mut()).await.unwrap();
    assert_eq!(response.output, original);
    let mut event = crate::urp::UrpStreamEvent::ResponseDone {
        output: original.clone(), outcome: None, usage: None, finish_reason: None, extra_body: HashMap::new()
    };
    transform.apply(UrpData::Stream(&mut event), Phase::Response, &context,
        config.as_ref(), transform.init_state().as_mut()).await.unwrap();
    let crate::urp::UrpStreamEvent::ResponseDone { output, .. } = event else { unreachable!() };
    assert_eq!(output, original);
}

#[tokio::test]
async fn appended_image_markdown_invalidates_original_text_signature_and_scores() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Responses, temp.path()).await;
    let mut response: crate::urp::UrpResponse = serde_json::from_value(json!({"id":"test-response", "model":"test", "output":[annotated_text(),
        {"type":"image", "role":"assistant", "source":{"type":"url", "url":"https://example.com/image.png"}}]})).unwrap();
    let registry = registry();
    let transform = registry.get("image_output_to_markdown").unwrap();
    let config = transform.parse_config(json!({})).unwrap();
    transform.apply(UrpData::Response(&mut response), Phase::Response, &context,
        config.as_ref(), transform.init_state().as_mut()).await.unwrap();
    let crate::urp::Node::Text { content, signature, logprobs, citations, .. } = &response.output[0] else { panic!("text") };
    assert_eq!(content, "hello![image](https://example.com/image.png)");
    assert_eq!(*signature, None);
    assert_eq!(*logprobs, None);
    assert_eq!(citations.len(), 1);
}

fn png(alpha: bool) -> String {
    let image = if alpha {
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([30, 90, 150, 255]),
        ))
    } else {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([30, 90, 150])))
    };
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    STANDARD.encode(bytes.into_inner())
}

#[tokio::test]
async fn anthropic_tool_cache_advances_to_final_tool_result() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Messages, temp.path()).await;
    let mut req = request(json!([
        {"type":"text", "role":"user", "content":"run again"},
        {"type":"tool_call", "call_id":"call_1", "name":"run", "arguments":"{}"},
        {"type":"tool_result", "call_id":"call_1", "content":[{"type":"text", "text":"result"}]}
    ]));
    apply_request("cache_anthropic_tool_use", &mut req, json!({}), &context).await;
    assert!(
        !serde_json::to_value(&req.input[0])
            .unwrap()
            .get("cache_control")
            .is_some()
    );
    assert_eq!(
        serde_json::to_value(&req.input[2])
            .unwrap()
            .get("cache_control"),
        Some(&json!({"type":"ephemeral"}))
    );
    let once = serde_json::to_value(&req).unwrap();
    apply_request("cache_anthropic_tool_use", &mut req, json!({}), &context).await;
    assert_eq!(serde_json::to_value(req).unwrap(), once);
}

#[tokio::test]
async fn anthropic_auto_cache_obeys_protocol_and_shared_slot_limit() {
    let temp = tempfile::tempdir().unwrap();
    let messages = context(ProviderType::Messages, temp.path()).await;
    let chat = context(ProviderType::ChatCompletion, temp.path()).await;
    let mut req = request(json!([]));
    apply_request("cache_anthropic_auto", &mut req, json!({}), &chat).await;
    assert!(!req.extra_body.contains_key("cache_control"));
    apply_request("cache_anthropic_auto", &mut req, json!({}), &messages).await;
    assert_eq!(
        req.extra_body.get("cache_control"),
        Some(&json!({"type":"ephemeral"}))
    );
    req.input = (0..3)
        .map(|_| {
            serde_json::from_value(json!({
        "type":"text", "role":"user", "content":"cached", "cache_control":{"type":"ephemeral"}
    })).unwrap()
        })
        .collect();
    req.input.push(text_node(OrdinaryRole::System, "system"));
    apply_request("cache_anthropic_system", &mut req, json!({}), &messages).await;
    assert!(
        !serde_json::to_value(req.input.last().unwrap())
            .unwrap()
            .get("cache_control")
            .is_some()
    );
    req.extra_body.remove("cache_control");
    req.input
        .last_mut()
        .unwrap()
        .extra_body_mut()
        .insert("cache_control".into(), json!({"type":"ephemeral"}));
    apply_request("cache_anthropic_auto", &mut req, json!({}), &messages).await;
    assert!(!req.extra_body.contains_key("cache_control"));
}

#[tokio::test]
async fn compression_rewrites_tool_result_and_function_image_mime_only() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Messages, temp.path()).await;
    let data = png(false);
    let args = json!({"nested":[format!("data:image/jpeg;base64,{data}"), {"type":"image", "source":{"type":"base64", "media_type":"image/jpeg", "data":data}, "detail":"high"}], "untouched":"keep"}).to_string();
    let custom_args = format!(" {{ \"url\":\"data:image/jpeg;base64,{data}\" }} ");
    let mut req = request(json!([
        {"type":"tool_result", "call_id":"call_1", "content":[{"type":"image", "source":{"type":"base64", "media_type":"image/jpeg", "data":data}}]},
        {"type":"tool_call", "call_id":"call_2", "name":"edit", "arguments":args},
        {"type":"tool_call", "tool_type":"custom", "call_id":"call_3", "name":"raw", "arguments":custom_args}
    ]));
    apply_request(
        "image_compress_input",
        &mut req,
        json!({"output_format":"png", "skip_if_smaller":false}),
        &context,
    )
    .await;
    let Node::ToolResult { content, .. } = &req.input[0] else {
        panic!()
    };
    let ToolResultContent::Image {
        source: crate::urp::ImageSource::Base64 { media_type, .. },
        ..
    } = &content[0]
    else {
        panic!()
    };
    assert_eq!(media_type, "image/png");
    let Node::ToolCall { arguments, .. } = &req.input[1] else {
        panic!()
    };
    let changed: Value = serde_json::from_str(arguments).unwrap();
    assert!(
        changed["nested"][0]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(changed["nested"][1]["source"]["media_type"], "image/png");
    assert_eq!(changed["nested"][1]["detail"], "high");
    assert_eq!(changed["untouched"], "keep");
    let Node::ToolCall { arguments, .. } = &req.input[2] else {
        panic!()
    };
    assert_eq!(arguments, &custom_args);
}

#[tokio::test]
async fn compression_preserves_alpha_and_all_images_when_request_has_mask() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Messages, temp.path()).await;
    let mut alpha = request(
        json!([{ "type":"image", "role":"user", "source":{"type":"base64", "media_type":"image/png", "data":png(true)} }]),
    );
    let before = serde_json::to_value(&alpha).unwrap();
    apply_request(
        "image_compress_input",
        &mut alpha,
        json!({"output_format":"jpg", "max_edge_px":1, "skip_if_smaller":false}),
        &context,
    )
    .await;
    assert_eq!(serde_json::to_value(alpha).unwrap(), before);
    let mut masked = request(json!([
        {"type":"image", "role":"user", "source":{"type":"base64", "media_type":"image/png", "data":png(false)}},
        {"type":"tool_result", "call_id":"call_1", "content":[{"type":"image", "metadata":{"image_mask":true}, "source":{"type":"base64", "media_type":"image/png", "data":png(false)}}]}
    ]));
    let before = serde_json::to_value(&masked).unwrap();
    apply_request(
        "image_compress_input",
        &mut masked,
        json!({"output_format":"jpg", "max_edge_px":1, "skip_if_smaller":false}),
        &context,
    )
    .await;
    assert_eq!(serde_json::to_value(masked).unwrap(), before);
}

#[tokio::test]
async fn field_set_null_condition_matches_only_present_null() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Responses, temp.path()).await;
    for initial in [None, Some(json!("priority")), Some(Value::Null)] {
        let mut req = request(json!([]));
        if let Some(value) = initial.clone() {
            req.extra_body.insert("service_tier".into(), value);
        }
        apply_request(
            "field_set",
            &mut req,
            json!({"path":"service_tier", "when_equals":null, "value":"default"}),
            &context,
        )
        .await;
        let expected = if initial == Some(Value::Null) {
            Some(json!("default"))
        } else {
            initial
        };
        assert_eq!(req.extra_body.get("service_tier"), expected.as_ref());
    }
}

#[tokio::test]
async fn output_compression_preserves_argument_fragments_and_rewrites_terminal_images() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(ProviderType::Responses, temp.path()).await;
    let transform = image_compress::ImageCompressOutputTransform;
    let config = transform
        .parse_config(json!({"output_format":"png", "skip_if_smaller":false}))
        .unwrap();
    let mut state = transform.init_state();
    let data = png(false);
    let arguments = json!({"image": format!("data:image/jpeg;base64,{data}")}).to_string();
    let mut start: UrpStreamEvent = serde_json::from_value(json!({
        "event":"node_start", "node_index":4,
        "header":{"type":"tool_call", "call_id":"call_4", "name":"edit"}
    }))
    .unwrap();
    transform
        .apply(
            UrpData::Stream(&mut start),
            Phase::Response,
            &context,
            config.as_ref(),
            state.as_mut(),
        )
        .await
        .unwrap();
    let partial = &arguments[..arguments.len() - 2];
    let mut delta: UrpStreamEvent = serde_json::from_value(json!({
        "event":"node_delta", "node_index":4,
        "delta":{"type":"tool_call_arguments", "arguments":partial}
    }))
    .unwrap();
    transform
        .apply(
            UrpData::Stream(&mut delta),
            Phase::Response,
            &context,
            config.as_ref(),
            state.as_mut(),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(delta).unwrap()["delta"]["arguments"],
        partial
    );
    let mut done: UrpStreamEvent = serde_json::from_value(json!({
        "event":"node_done", "node_index":4,
        "node":{"type":"tool_call", "call_id":"call_4", "name":"edit", "arguments":arguments}
    }))
    .unwrap();
    transform
        .apply(
            UrpData::Stream(&mut done),
            Phase::Response,
            &context,
            config.as_ref(),
            state.as_mut(),
        )
        .await
        .unwrap();
    let UrpStreamEvent::NodeDone {
        node: Node::ToolCall { arguments, .. },
        ..
    } = done
    else {
        panic!()
    };
    let rewritten: Value = serde_json::from_str(&arguments).unwrap();
    assert!(
        rewritten["image"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
}
