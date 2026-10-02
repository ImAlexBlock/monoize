use super::*;

fn cfg(names: &[&str]) -> Config {
    parse_names(Some(names.iter().map(|name| (*name).to_string()).collect())).unwrap()
}

fn request(tools: Value, input: Value, choice: Value) -> UrpRequest {
    serde_json::from_value(json!({
        "model": "test", "tools": tools, "input": input, "tool_choice": choice,
    }))
    .unwrap()
}

fn custom(name: &str) -> Value {
    json!({"type":"custom", "custom":{"name":name,"format":{"type":"text"}}})
}

fn function(name: &str) -> Value {
    json!({"type":"function", "function":{"name":name,"parameters":{"type":"object"}}})
}

fn call(name: &str, call_id: &str, arguments: &str, namespace: Option<&str>) -> Node {
    serde_json::from_value(json!({
        "type":"tool_call", "tool_type":"function", "name":name,
        "call_id":call_id, "arguments":arguments, "namespace":namespace,
    }))
    .unwrap()
}

fn start(index: u32, node: &Node) -> UrpStreamEvent {
    UrpStreamEvent::NodeStart {
        node_index: index,
        header: tool_header(node).unwrap(),
        extra_body: HashMap::new(),
    }
}

fn delta(index: u32, arguments: &str) -> UrpStreamEvent {
    UrpStreamEvent::NodeDelta {
        node_index: index,
        delta: NodeDelta::ToolCallArguments {
            arguments: arguments.to_string(),
        },
        usage: None,
        extra_body: HashMap::new(),
    }
}

fn done(index: u32, node: Node) -> UrpStreamEvent {
    UrpStreamEvent::NodeDone {
        node_index: index,
        node,
        usage: None,
        extra_body: HashMap::new(),
    }
}

fn response_done(output: Vec<Node>) -> UrpStreamEvent {
    UrpStreamEvent::ResponseDone {
        outcome: None,
        finish_reason: None,
        usage: None,
        output,
        extra_body: HashMap::new(),
    }
}

fn stream(
    event: UrpStreamEvent,
    cfg: &Config,
    conversions: &CustomToolConversions,
    state: &mut StreamState,
) -> Vec<UrpStreamEvent> {
    let mut event = event;
    apply_stream(&mut event, cfg, conversions, state);
    state.finalize_stream_event(event)
}

fn value<T: serde::Serialize>(item: &T) -> Value {
    serde_json::to_value(item).unwrap()
}

#[test]
fn wildcard_restores_only_converted_custom_tools() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([custom("patch"), function("read")]),
        json!([]),
        Value::Null,
    );
    let original_function = value(&req.tools.as_ref().unwrap()[1]);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    assert_eq!(value(&req.tools.as_ref().unwrap()[1]), original_function);
    let native = call("read", "native", r#"{"input":"literal native JSON"}"#, None);
    let mut output = vec![
        call("patch", "converted", &wrap_input("raw input"), None),
        native.clone(),
    ];
    let mut calls = HashMap::new();
    for node in &mut output {
        convert_node_response(node, &cfg, &conversions, &mut calls);
    }
    assert_eq!(value(&output[0])["tool_type"], "custom");
    assert_eq!(value(&output[0])["arguments"], "raw input");
    assert_eq!(output[1], native);
}

#[test]
fn response_only_and_other_requests_do_not_claim_function_calls() {
    let cfg = cfg(&["*"]);
    let original = call("apply_patch", "a", &wrap_input("*** Begin Patch ***"), None);
    let mut node = original.clone();
    assert!(!convert_node_response(
        &mut node,
        &cfg,
        &CustomToolConversions::default(),
        &mut HashMap::new()
    ));
    assert_eq!(node, original);
}

#[test]
fn namespaces_recurse_and_flat_wire_names_restore_only_unambiguous_identity() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([
            {"type":"namespace","name":"editing","tools":[
                custom("patch"),
                {"type":"namespace","name":"inner","tools":[custom("deep")]}
            ]},
            {"type":"function","namespace":"native","function":{"name":"patch"}},
        ]),
        json!([]),
        Value::Null,
    );
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let tools = value(&req.tools);
    assert_eq!(tools[0]["tools"][0]["type"], "function");
    assert_eq!(tools[0]["tools"][1]["tools"][0]["type"], "function");
    assert!(
        conversions
            .resolve(Some("editing"), "patch", &cfg)
            .is_some()
    );
    assert!(conversions.resolve(Some("native"), "patch", &cfg).is_none());
    assert!(conversions.resolve(None, "patch", &cfg).is_none());
    let mut deep = call("deep", "d", &wrap_input("text"), None);
    assert!(convert_node_response(
        &mut deep,
        &cfg,
        &conversions,
        &mut HashMap::new()
    ));
    assert_eq!(value(&deep)["namespace"], "inner");
    assert_eq!(value(&deep)["arguments"], "text");
}

#[test]
fn same_identity_native_function_collision_rejects_before_request_mutation() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([custom("patch"), function("patch")]),
        json!([]),
        Value::Null,
    );
    let original = value(&req);
    assert!(apply_request(&mut req, &cfg, &mut CustomToolConversions::default()).is_err());
    assert_eq!(value(&req), original);
}

#[test]
fn custom_json_history_is_wrapped_once_and_results_follow_call_ids() {
    let cfg = cfg(&["*"]);
    let raw = r#"{"input":"a literal JSON input","count":2}"#;
    let mut req = request(
        json!([]),
        json!([
            {"type":"tool_result","tool_type":"custom","call_id":"a","content":[{"type":"text","text":"ok"}],"extra":17},
            {"type":"tool_call","tool_type":"custom","call_id":"a","name":"raw","arguments":raw},
            {"type":"tool_result","tool_type":"custom","call_id":"unrelated","content":[]},
        ]),
        Value::Null,
    );
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let after_first = value(&req);
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    assert_eq!(value(&req), after_first);
    assert_eq!(after_first["input"][0]["tool_type"], "function");
    assert_eq!(after_first["input"][0]["extra"], 17);
    assert_eq!(after_first["input"][2]["tool_type"], "custom");
    let wrapped = after_first["input"][1]["arguments"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(wrapped).unwrap()["input"],
        raw
    );
    let mut returned = call("raw", "new", wrapped, None);
    assert!(convert_node_response(
        &mut returned,
        &cfg,
        &conversions,
        &mut HashMap::new()
    ));
    assert_eq!(value(&returned)["arguments"], raw);
}

#[test]
fn selectors_convert_supported_shapes_without_touching_extension_payloads() {
    let cfg = cfg(&["*"]);
    let extension = json!({"type":"custom","name":"patch","custom":{"name":"patch"}});
    let mut req = request(
        json!([custom("patch"), function("read")]),
        json!([]),
        json!({
            "type":"allowed_tools",
            "allowed_tools":{"mode":"required","tools":[
                {"type":"custom","custom":{"name":"patch","opaque":7}},
                {"type":"custom","name":"patch"},
                {"type":"function","function":{"name":"read"}},
                {"type":"custom","name":"unknown"},
            ]},
            "extension":extension,
        }),
    );
    apply_request(&mut req, &cfg, &mut CustomToolConversions::default()).unwrap();
    let choice = value(&req.tool_choice);
    assert_eq!(
        choice["allowed_tools"]["tools"][0],
        json!({"type":"function","function":{"name":"patch","opaque":7}})
    );
    assert_eq!(choice["allowed_tools"]["tools"][1]["type"], "function");
    assert_eq!(
        choice["allowed_tools"]["tools"][2]["function"]["name"],
        "read"
    );
    assert_eq!(choice["allowed_tools"]["tools"][3]["type"], "custom");
    assert_eq!(choice["extension"], extension);
}

#[test]
fn explicit_namespace_selectors_and_results_are_preserved() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([
            {"type":"namespace","name":"ns","tools":[custom("patch")]},
        ]),
        json!([
            {"type":"tool_result","tool_type":"custom","namespace":"ns","name":"patch","call_id":"a","content":[]},
            {"type":"tool_result","tool_type":"custom","namespace":"other","name":"patch","call_id":"b","content":[]},
        ]),
        json!({"type":"custom","namespace":"ns","custom":{"name":"patch"}}),
    );
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    assert_eq!(
        value(&req.tool_choice),
        json!({"type":"function","namespace":"ns","function":{"name":"patch"}})
    );
    assert_eq!(value(&req.input[0])["tool_type"], "function");
    assert_eq!(value(&req.input[1])["tool_type"], "custom");
}

#[test]
fn generic_schema_does_not_instruct_apply_patch_or_keep_grammar() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([{
            "type":"custom", "namespace":"ns", "outer":"kept", "custom":{
                "name":"shell", "description":"Run a command", "format":{"type":"grammar"},
                "defer_loading":true,
            },
        }]),
        json!([]),
        Value::Null,
    );
    apply_request(&mut req, &cfg, &mut CustomToolConversions::default()).unwrap();
    let tool = value(&req.tools)[0].clone();
    assert_eq!(tool["function"]["strict"], false);
    assert_eq!(tool["function"]["defer_loading"], true);
    assert_eq!(tool["namespace"], "ns");
    assert_eq!(tool["outer"], "kept");
    assert!(tool.get("custom").is_none());
    assert!(tool["function"].get("format").is_none());
    assert!(
        !tool["function"]["parameters"]
            .to_string()
            .contains("apply_patch")
    );
}

#[test]
fn fragmented_stream_never_emits_json_wrapper_or_changes_native_function_arguments() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([custom("patch"), function("read")]),
        json!([]),
        Value::Null,
    );
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let raw = "你好\nquote: \" emoji: 🦀 slash: \\";
    let wrapped = wrap_input(raw);
    let custom_node = call("patch", "c", &wrapped, None);
    let native_node = call("read", "n", r#"{"input":"ordinary"}"#, None);
    let mut state = StreamState::default();
    let started = stream(start(4, &custom_node), &cfg, &conversions, &mut state);
    assert_eq!(value(&started[0])["header"]["tool_type"], "custom");
    stream(start(5, &native_node), &cfg, &conversions, &mut state);
    for ch in wrapped.chars() {
        assert!(stream(delta(4, &ch.to_string()), &cfg, &conversions, &mut state).is_empty());
    }
    let native_delta = delta(5, r#"{"input":"ordinary"}"#);
    let expected = value(&native_delta);
    assert_eq!(
        value(&stream(native_delta, &cfg, &conversions, &mut state)[0]),
        expected
    );
    let completed = stream(done(4, custom_node.clone()), &cfg, &conversions, &mut state);
    assert_eq!(completed.len(), 2);
    assert_eq!(value(&completed[0])["delta"]["arguments"], raw);
    assert_eq!(value(&completed[1])["node"]["arguments"], raw);
    let finish = stream(
        response_done(vec![custom_node, native_node.clone()]),
        &cfg,
        &conversions,
        &mut state,
    );
    assert_eq!(finish.len(), 1);
    assert_eq!(value(&finish[0])["output"][0]["arguments"], raw);
    assert_eq!(value(&finish[0])["output"][1], value(&native_node));
}

#[test]
fn response_done_without_node_done_flushes_converted_node_before_terminal() {
    let cfg = cfg(&["*"]);
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let node = call("raw", "a", &wrap_input("full text"), None);
    let mut state = StreamState::default();
    stream(start(7, &node), &cfg, &conversions, &mut state);
    stream(
        delta(7, "{\"input\":\"full text\"}"),
        &cfg,
        &conversions,
        &mut state,
    );
    let output = stream(response_done(vec![node]), &cfg, &conversions, &mut state);
    assert_eq!(output.len(), 3);
    assert_eq!(value(&output[0])["delta"]["arguments"], "full text");
    assert_eq!(value(&output[1])["node"]["arguments"], "full text");
    assert_eq!(value(&output[2])["output"][0]["arguments"], "full text");
}

#[test]
fn response_done_only_can_supply_the_entire_converted_tool_lifecycle() {
    let cfg = cfg(&["*"]);
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let output = stream(
        response_done(vec![call("raw", "a", &wrap_input("text"), None)]),
        &cfg,
        &conversions,
        &mut StreamState::default(),
    );
    assert_eq!(output.len(), 4);
    assert!(matches!(output[0], UrpStreamEvent::NodeStart { .. }));
    assert!(matches!(output[1], UrpStreamEvent::NodeDelta { .. }));
    assert!(matches!(output[2], UrpStreamEvent::NodeDone { .. }));
    assert!(matches!(output[3], UrpStreamEvent::ResponseDone { .. }));
}

#[test]
fn error_discards_partial_wrapper_without_fabricating_completed_tools() {
    let cfg = cfg(&["*"]);
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let node = call("raw", "a", "", None);
    let mut state = StreamState::default();
    stream(start(0, &node), &cfg, &conversions, &mut state);
    assert!(
        stream(
            delta(0, "{\"input\":\"incomplete"),
            &cfg,
            &conversions,
            &mut state
        )
        .is_empty()
    );
    let error = UrpStreamEvent::Error {
        code: None,
        message: "failed".to_string(),
        extra_body: HashMap::new(),
    };
    assert_eq!(stream(error, &cfg, &conversions, &mut state).len(), 1);
    assert!(state.calls.is_empty());
    assert!(stream(response_done(vec![node]), &cfg, &conversions, &mut state).is_empty());
}

#[test]
fn unnamed_function_header_is_buffered_until_completion_identifies_owned_tool() {
    let cfg = cfg(&["*"]);
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let unnamed = Node::ToolCall {
        namespace: None,
        signature: None,
        id: None,
        tool_type: ToolCallType::Function,
        call_id: "a".to_string(),
        name: "raw".to_string(),
        arguments: wrap_input("text"),
        extra_body: HashMap::new(),
    };
    let unknown_header = NodeHeader::ToolCall {
        namespace: None,
        signature: None,
        id: None,
        tool_type: ToolCallType::Function,
        call_id: "a".to_string(),
        name: String::new(),
    };
    let mut state = StreamState::default();
    let mut start_event = UrpStreamEvent::NodeStart {
        node_index: 3,
        header: unknown_header,
        extra_body: {
            let mut m = HashMap::new();
            m.insert("trace".to_string(), json!(1));
            m
        },
    };
    apply_stream(&mut start_event, &cfg, &conversions, &mut state);
    assert!(state.finalize_stream_event(start_event).is_empty());
    let mut delta_event = delta(3, &wrap_input("text"));
    apply_stream(&mut delta_event, &cfg, &conversions, &mut state);
    assert!(state.finalize_stream_event(delta_event).is_empty());
    let output = stream(done(3, unnamed), &cfg, &conversions, &mut state);
    assert_eq!(output.len(), 3);
    assert_eq!(value(&output[0])["header"]["tool_type"], "custom");
    assert_eq!(value(&output[0])["trace"], 1);
    assert_eq!(value(&output[2])["node"]["tool_type"], "custom");
    assert_eq!(value(&output[2])["node"]["arguments"], "text");
}

#[test]
fn patch_normalization_matches_delta_node_done_and_response_done() {
    let cfg = cfg(&["apply_patch"]);
    let mut req = request(json!([custom("apply_patch")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let raw = "*** Begin Patch ***\n*** Add File: x\nhello\n*** End Patch ***";
    let node = call("apply_patch", "a", &wrap_input(raw), None);
    let mut state = StreamState::default();
    stream(start(0, &node), &cfg, &conversions, &mut state);
    let output = stream(done(0, node.clone()), &cfg, &conversions, &mut state);
    let normalized = "*** Begin Patch\n*** Add File: x\n+hello\n*** End Patch\n";
    assert_eq!(value(&output[0])["delta"]["arguments"], normalized);
    assert_eq!(value(&output[1])["node"]["arguments"], normalized);
    let final_output = stream(response_done(vec![node]), &cfg, &conversions, &mut state);
    assert_eq!(
        value(&final_output[0])["output"][0]["arguments"],
        normalized
    );
}
