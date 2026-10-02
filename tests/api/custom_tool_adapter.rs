use super::*;

const MODEL: &str = "custom-adapter-fixture";
const CUSTOM_INPUT: &str = "first line\n中文 \"quoted\" \\ second line";
const NATIVE_ARGUMENTS: &str = r#"{"input":"native function input"}"#;

async fn custom_adapter_context(
    buffered: bool,
    response_only: bool,
) -> (TestContext, CapturedBodies) {
    async fn upstream(
        axum::extract::State(captured): axum::extract::State<CapturedBodies>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        captured.lock().unwrap().push(("chat".into(), body.clone()));
        if body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool.get("custom").is_some())
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": {"message": "Unknown parameter: 'tools[0].custom'."}})),
            )
                .into_response();
        }
        let arguments = json!({"input": CUSTOM_INPUT}).to_string();
        let usage = json!({"prompt_tokens": 10, "completion_tokens": 12, "total_tokens": 22});
        if body["stream"] == true {
            let chunk = |delta: Value, finish: Value| {
                json!({
                    "id":"custom-bridge-response", "object":"chat.completion.chunk", "model":MODEL,
                    "choices":[{"index":0,"delta":delta,"finish_reason":finish}]
                })
            };
            let mut frames = vec![chunk(json!({"content":"text remains live"}), Value::Null)];
            frames.push(chunk(json!({"tool_calls":[
                {"index":0,"id":"custom-call","type":"function","function":{"name":"grammar_tool","arguments":""}},
                {"index":1,"id":"native-call","type":"function","function":{"name":"native_tool","arguments":""}}
            ]}), Value::Null));
            let mut native_sent = false;
            for character in arguments.chars() {
                frames.push(chunk(json!({"tool_calls":[{"index":0,"function":{"arguments":character.to_string()}}]}), Value::Null));
                if !native_sent {
                    frames.push(chunk(json!({"tool_calls":[{"index":1,"function":{"arguments":NATIVE_ARGUMENTS}}]}),Value::Null));
                    native_sent = true;
                }
            }
            frames.push(chunk(json!({}), json!("tool_calls")));
            frames.push(json!({"id":"custom-bridge-response","object":"chat.completion.chunk","model":MODEL,"choices":[],"usage":usage}));
            let mut output = frames
                .into_iter()
                .map(|frame| format!("data: {frame}\n\n"))
                .collect::<String>();
            output.push_str("data: [DONE]\n\n");
            return ([(CONTENT_TYPE, "text/event-stream")], output).into_response();
        }
        Json(json!({"id":"custom-bridge-response","object":"chat.completion","model":MODEL,
            "choices":[{"index":0,"message":{"role":"assistant","content":"text remains live","tool_calls":[
                {"id":"custom-call","type":"function","function":{"name":"grammar_tool","arguments":arguments}},
                {"id":"native-call","type":"function","function":{"name":"native_tool","arguments":NATIVE_ARGUMENTS}}
            ]},"finish_reason":"tool_calls"}],"usage":usage})).into_response()
    }
    let ctx = setup().await;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(captured.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut transforms = vec![];
    if !response_only {
        transforms.push(json!({"transform":"field_custom_tools_to_function","phase":"request","config":{"names":["*"]}}));
    }
    transforms.push(json!({"transform":"field_custom_tools_to_function","phase":"response","config":{"names":["*"]}}));
    if buffered {
        transforms
            .push(json!({"transform":"image_markdown_to_output","phase":"response","config":{}}));
    }
    ctx.state.monoize_store.create_provider(serde_json::from_value(json!({
        "name":"custom-adapter-fixture-provider","confirm_public_exposure":true,
        "pricing_profile":"default","transforms":transforms,
        "channel":{"name":"custom-adapter-fixture-channel","provider_type":"chat_completion",
            "base_url":format!("http://{address}"),"api_key":"fixture-key","models":{(MODEL):{}}}
    })).unwrap()).await.unwrap();
    seed_test_model_pricing(&ctx.state, &[MODEL]).await;
    (ctx, captured)
}

fn request(stream: bool) -> Value {
    json!({"model":MODEL,"stream":stream,"input":[
        {"type":"custom_tool_call","call_id":"history-custom","name":"grammar_tool","input":"{\"input\":\"literal custom bytes\"}"},
        {"type":"custom_tool_call_output","call_id":"history-custom","output":"done"},
        {"type":"message","role":"user","content":"Use both tools."}
    ],"tools":[
        {"type":"custom","name":"grammar_tool","format":{"type":"text"}},
        {"type":"function","name":"native_tool","parameters":{"type":"object","properties":{"input":{"type":"string"}}}}
    ],"tool_choice":{"type":"custom","name":"grammar_tool"}})
}

fn assert_upstream(body: &Value) {
    let tools = body["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .all(|tool| tool["type"] == "function" && tool.get("custom").is_none()),
        "{body}"
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type":"function","function":{"name":"grammar_tool"}})
    );
    assert_eq!(
        tools[0]["function"]["parameters"]["required"],
        json!(["input"])
    );
    let messages = body["messages"].as_array().unwrap();
    let call = messages
        .iter()
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten())
        .find(|call| call["id"] == "history-custom")
        .unwrap();
    assert_eq!(call["type"], "function");
    let arguments: Value =
        serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(arguments["input"], "{\"input\":\"literal custom bytes\"}");
    assert!(messages.iter().any(|message|message["role"]=="tool" && message["tool_call_id"]=="history-custom"));
}

fn assert_output(output: &Value) {
    let items = output.as_array().unwrap();
    let custom = items
        .iter()
        .find(|item| item["call_id"] == "custom-call")
        .unwrap();
    let native = items
        .iter()
        .find(|item| item["call_id"] == "native-call")
        .unwrap();
    assert_eq!(custom["type"], "custom_tool_call", "{output}");
    assert_eq!(custom["input"], CUSTOM_INPUT, "{output}");
    assert_eq!(native["type"], "function_call", "{output}");
    assert_eq!(native["arguments"], NATIVE_ARGUMENTS, "{output}");
}

#[tokio::test]
async fn custom_tool_adapter_round_trip_nonstream_preserves_native_function() {
    let (ctx, captured) = custom_adapter_context(false, false).await;
    let (status, body) = json_post(&ctx, "/v1/responses", request(false)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_upstream(&captured.lock().unwrap()[0].1);
    assert_output(&body["output"]);
}

async fn verify_stream() {
    let (ctx, captured) = custom_adapter_context(false, false).await;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, ctx.auth_header.clone())
        .body(Body::from(request(true).to_string()))
        .unwrap();
    let resp = ctx.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let frames = parse_responses_sse_json(&text);
    let custom_delta = frames
        .iter()
        .filter(|(event, _)| event == "response.custom_tool_call_input.delta")
        .filter_map(|(_, payload)| payload["delta"].as_str())
        .collect::<String>();
    assert_eq!(custom_delta, CUSTOM_INPUT, "{text}");
    let native_delta = frames
        .iter()
        .filter(|(event, _)| event == "response.function_call_arguments.delta")
        .filter_map(|(_, payload)| payload["delta"].as_str())
        .collect::<String>();
    assert_eq!(native_delta, NATIVE_ARGUMENTS, "{text}");
    let done = frames
        .iter()
        .find(|(event, _)| event == "response.completed")
        .expect(&text);
    assert_output(&done.1["response"]["output"]);
    assert_upstream(&captured.lock().unwrap()[0].1);
    let ordinary = frames
        .iter()
        .position(|(event, _)| event == "response.output_text.delta")
        .unwrap();
    let custom = frames
        .iter()
        .position(|(event, _)| event == "response.custom_tool_call_input.delta")
        .unwrap();
    assert!(ordinary < custom, "{text}");
}

async fn verify_buffered_chat_stream() {
    let (ctx, captured) = custom_adapter_context(true, false).await;
    let body = json!({
        "model": MODEL,
        "stream": true,
        "messages": [
            {"role": "assistant", "content": null, "tool_calls": [{
                "id": "history-custom", "type": "custom",
                "custom": {"name": "grammar_tool", "input": "{\"input\":\"literal custom bytes\"}"}
            }]},
            {"role": "tool", "tool_call_id": "history-custom", "content": "done"},
            {"role": "user", "content": "Use both tools."}
        ],
        "tools": [
            {"type": "custom", "custom": {"name": "grammar_tool", "format": {"type": "text"}}},
            {"type": "function", "function": {"name": "native_tool", "parameters": {"type": "object"}}}
        ],
        "tool_choice": {"type": "custom", "custom": {"name": "grammar_tool"}}
    });
    let (status, response) = json_post(&ctx, "/v1/chat/completions", body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let frames = parse_sse_frames(&response);
    assert_eq!(
        frames.iter().filter(|(_, data)| data == "[DONE]").count(),
        1,
        "{response}"
    );
    let chunks = frames
        .into_iter()
        .filter(|(_, data)| data != "[DONE]")
        .map(|(event, data)| {
            assert!(event.is_none(), "{response}");
            serde_json::from_str::<Value>(&data).expect("Chat SSE chunk is JSON")
        })
        .collect::<Vec<_>>();
    let text = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect::<String>();
    assert_eq!(text, "text remains live", "{response}");
    let finish_reasons = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["finish_reason"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(finish_reasons, vec!["tool_calls"], "{response}");
    let calls = chunks
        .iter()
        .flat_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .collect::<Vec<_>>();
    let mut indices = std::collections::HashSet::new();
    for (id, kind, name, argument_key, expected) in [
        (
            "custom-call",
            "custom",
            "grammar_tool",
            "input",
            CUSTOM_INPUT,
        ),
        (
            "native-call",
            "function",
            "native_tool",
            "arguments",
            NATIVE_ARGUMENTS,
        ),
    ] {
        let header = calls
            .iter()
            .find(|call| call["id"] == id)
            .expect("expected tool call header");
        assert_eq!(header["type"], kind, "{response}");
        assert_eq!(header[kind]["name"], name, "{response}");
        let index = header["index"].as_u64().expect("tool call index");
        assert!(
            indices.insert(index),
            "tool call indices differ: {response}"
        );
        let mut arguments = String::new();
        for call in calls.iter().filter(|call| call["index"] == index) {
            let other_kind = if kind == "custom" {
                "function"
            } else {
                "custom"
            };
            assert!(call.get(other_kind).is_none(), "{response}");
            if let Some(fragment) = call[kind][argument_key].as_str() {
                arguments.push_str(fragment);
            }
        }
        assert_eq!(arguments, expected, "{response}");
    }
    assert!(
        calls
            .iter()
            .all(|call| indices.contains(&call["index"].as_u64().expect("tool call index"))),
        "unexpected tool call: {response}"
    );
    let upstream = captured.lock().unwrap();
    assert_eq!(upstream.len(), 1, "{upstream:?}");
    assert_eq!(
        upstream[0].1["stream"], false,
        "buffered upstream request: {upstream:?}"
    );
    assert_upstream(&upstream[0].1);
}

#[tokio::test]
async fn custom_tool_adapter_fragmented_stream_preserves_native_function() {
    verify_stream().await;
}

#[tokio::test]
async fn custom_tool_adapter_buffered_stream_retains_request_ownership() {
    verify_buffered_chat_stream().await;
}

#[tokio::test]
async fn custom_tool_adapter_response_only_does_not_reclassify_native_functions() {
    let (ctx, _) = custom_adapter_context(false, true).await;
    let (status, body) = json_post(
        &ctx,
        "/v1/responses",
        json!({"model":MODEL,"input":"Use the functions.","tools":[
            {"type":"function","name":"grammar_tool","parameters":{"type":"object"}},
            {"type":"function","name":"native_tool","parameters":{"type":"object"}}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    let output = body["output"].as_array().unwrap();
    assert!(
        output
            .iter()
            .filter(|item| item.get("call_id").is_some())
            .all(|item| item["type"] == "function_call"),
        "{body}"
    );
}
