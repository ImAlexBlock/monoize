use monoize::urp::{decode, encode};
use serde_json::json;

#[test]
fn gemini_sampling_maps_to_chat_controls() {
    let request = decode::gemini::decode_request(&json!({
        "model": "gemini", "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
        "generationConfig": {"seed": 42, "presencePenalty": 0.5, "frequencyPenalty": 0.25}
    })).unwrap();
    let wire = encode::openai_chat::encode_request(&request, "chat-model");
    assert_eq!(wire["seed"], 42);
    assert_eq!(wire["presence_penalty"], 0.5);
    assert_eq!(wire["frequency_penalty"], 0.25);
}

#[test]
fn client_context_never_reaches_the_provider() {
    let request = decode::openai_responses::decode_request(&json!({
        "model": "gpt", "input": "hello",
        "context": {"username": "forged", "api_key_id": "forged"}
    })).unwrap();
    let wire = encode::openai_responses::encode_request(&request, "gpt");
    assert!(wire.get("context").is_none(), "{wire}");
}
