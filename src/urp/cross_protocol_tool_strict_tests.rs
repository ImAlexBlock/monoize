use super::{ProviderProtocol, UrpRequest, decode, encode};
use serde_json::{Value, json};

fn schema() -> Value {
    json!({"type": "object", "properties": {"query": {"type": "string"}}})
}

fn chat_request(strict: Option<bool>, legacy: bool) -> UrpRequest {
    let mut function = json!({"name": "lookup", "parameters": schema()});
    if let Some(strict) = strict {
        function["strict"] = json!(strict);
    }
    let mut body = json!({"model": "test", "messages": [{"role": "user", "content": "lookup"}]});
    if legacy {
        body["functions"] = json!([function]);
    } else {
        body["tools"] = json!([{"type": "function", "function": function}]);
    }
    decode::openai_chat::decode_request(&body).unwrap()
}

#[test]
fn chat_functions_keep_optional_parameters_when_sent_to_responses() {
    for legacy in [false, true] {
        let req = chat_request(None, legacy);
        let tool = &req.tools.as_ref().unwrap()[0];
        assert_eq!(tool.origin_protocol, Some(ProviderProtocol::ChatCompletion));
        let encoded = encode::openai_responses::encode_request(&req, "upstream");
        assert_eq!(encoded["tools"][0]["strict"], false);
        assert_eq!(encoded["tools"][0]["parameters"], schema());
        let native = encode::openai_chat::encode_request(&req, "upstream");
        let function = if legacy {
            &native["functions"][0]
        } else {
            &native["tools"][0]["function"]
        };
        assert!(function.get("strict").is_none());
    }
}

#[test]
fn messages_and_gemini_functions_do_not_acquire_responses_strict_default() {
    let requests = [
        (
            ProviderProtocol::Messages,
            decode::anthropic::decode_request(&json!({
                "model": "test", "max_tokens": 100,
                "messages": [{"role": "user", "content": "lookup"}],
                "tools": [{"name": "lookup", "input_schema": schema()}]
            })).unwrap(),
        ),
        (
            ProviderProtocol::Gemini,
            decode::gemini::decode_request(&json!({
                "model": "test", "contents": [{"role": "user", "parts": [{"text": "lookup"}]}],
                "tools": [{"functionDeclarations": [{"name": "lookup", "parametersJsonSchema": schema()}]}]
            })).unwrap(),
        ),
    ];
    for (origin, req) in requests {
        assert_eq!(req.tools.as_ref().unwrap()[0].origin_protocol, Some(origin));
        let encoded = encode::openai_responses::encode_request(&req, "upstream");
        assert_eq!(encoded["tools"][0]["strict"], false);
        assert_eq!(encoded["tools"][0]["parameters"], schema());
        let messages = encode::anthropic::encode_request(&req, "upstream");
        assert!(messages["tools"][0].get("strict").is_none());
    }
}

#[test]
fn explicit_strict_values_survive_cross_protocol_conversion() {
    for strict in [false, true] {
        let req = chat_request(Some(strict), false);
        let encoded = encode::openai_responses::encode_request(&req, "upstream");
        assert_eq!(encoded["tools"][0]["strict"], strict);
        assert_eq!(encoded["tools"][0]["parameters"], schema());
    }
}

#[test]
fn responses_preserves_native_strict_defaults_and_nested_namespaces() {
    let mut req = decode::openai_responses::decode_request(&json!({
        "model": "test", "input": "lookup",
        "tools": [{"type": "namespace", "name": "inventory", "tools": [
            {"type": "function", "name": "lookup", "parameters": schema()},
            {"type": "function", "name": "search", "parameters": schema(), "strict": true},
            {"type": "function", "name": "scan", "parameters": schema(), "strict": false}
        ]}]
    }))
    .unwrap();
    let tools = req.tools.as_ref().unwrap()[0].tools.as_ref().unwrap();
    assert!(
        tools
            .iter()
            .all(|tool| tool.origin_protocol == Some(ProviderProtocol::Responses))
    );
    let native = encode::openai_responses::encode_request(&req, "upstream");
    assert!(native["tools"][0]["tools"][0].get("strict").is_none());
    assert_eq!(native["tools"][0]["tools"][1]["strict"], true);
    assert_eq!(native["tools"][0]["tools"][2]["strict"], false);

    req.tools.as_mut().unwrap()[0].set_function_origin(ProviderProtocol::ChatCompletion);
    let bridged = encode::openai_responses::encode_request(&req, "upstream");
    assert_eq!(bridged["tools"][0]["tools"][0]["strict"], false);
    assert_eq!(bridged["tools"][0]["tools"][0]["parameters"], schema());
    assert_eq!(bridged["tools"][0]["tools"][1]["strict"], true);
}
