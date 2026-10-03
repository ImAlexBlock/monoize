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
fn owned_custom_arguments_decode_json_strings_without_reparsing_the_inner_value() {
    let cfg = cfg(&["*"]);
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    for raw in ["多行\ninput", r#"{"input":"literal JSON"}"#] {
        let mut node = call("raw", "a", &json!(raw).to_string(), None);
        assert!(convert_node_response(
            &mut node,
            &cfg,
            &conversions,
            &mut HashMap::new()
        ));
        assert_eq!(value(&node)["arguments"], raw);
    }
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
fn converted_history_clears_custom_item_ids_without_changing_correlation_or_metadata() {
    let cfg = cfg(&["patch"]);
    let raw = r#"{"input":"literal custom JSON"}"#;
    let mut req = request(
        json!([{"type":"namespace","name":"tools","tools":[custom("patch")]}]),
        json!([
            {"type":"tool_result","tool_type":"custom","id":"ctco_01a1011e-2686-7731-aa8f-2dc4d235dee0",
                "call_id":"history-call","namespace":"tools","name":"patch",
                "content":[{"type":"text","text":"done","future_content":7}],"future_item":{"keep":true}},
            {"type":"tool_call","tool_type":"custom","id":"ctc_history","call_id":"history-call",
                "namespace":"tools","name":"patch","arguments":raw,"future_item":{"keep":true}},
            {"type":"tool_call","tool_type":"function","id":"fc_native","call_id":"native-call",
                "namespace":"tools","name":"read","arguments":"{}"},
            {"type":"tool_result","tool_type":"function","id":"fco_native","call_id":"native-call",
                "content":[{"type":"text","text":"native result"}]},
            {"type":"tool_call","tool_type":"custom","id":"ctc_unselected","call_id":"unselected-call",
                "namespace":"tools","name":"unselected","arguments":"unchanged"},
            {"type":"tool_result","tool_type":"custom","id":"ctco_unselected","call_id":"unselected-call",
                "content":[{"type":"text","text":"unselected result"}]}
        ]),
        Value::Null,
    );
    let original = value(&req);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let transformed = value(&req);
    for index in 0..2 {
        let mut expected = original["input"][index].clone();
        expected.as_object_mut().unwrap().remove("id");
        expected["tool_type"] = json!("function");
        if index == 1 {
            expected["arguments"] = json!(wrap_input(raw));
        }
        assert_eq!(transformed["input"][index], expected);
    }
    for index in 2..6 {
        assert_eq!(transformed["input"][index], original["input"][index]);
    }
    let wire = crate::urp::encode::openai_responses::encode_request(&req, "test");
    for index in 0..2 {
        assert!(wire["input"][index].get("id").is_none(), "{wire}");
        assert_eq!(wire["input"][index]["call_id"], "history-call");
        assert_eq!(wire["input"][index]["namespace"], "tools");
        assert_eq!(wire["input"][index]["future_item"], json!({"keep":true}));
    }
    assert_eq!(wire["input"][0]["type"], "function_call_output");
    assert_eq!(wire["input"][1]["type"], "function_call");
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    assert_eq!(value(&req), transformed);
    assert_eq!(
        crate::urp::encode::openai_responses::encode_request(&req, "test"),
        wire
    );
}

#[test]
fn only_descriptor_matched_orphan_result_loses_its_custom_item_id() {
    let cfg = cfg(&["patch"]);
    let mut req = request(
        json!([{"type":"namespace","name":"tools","tools":[custom("patch")]}]),
        json!([
            {"type":"tool_result","tool_type":"custom","id":"ctco_orphan","call_id":"orphan-call",
                "namespace":"tools","name":"patch","content":[{"type":"text","text":"done"}],"trace":8},
            {"type":"tool_result","tool_type":"custom","id":"ctco_other","call_id":"other-call",
                "namespace":"other","name":"patch","content":[],"trace":9},
            {"type":"tool_result","tool_type":"custom","id":"ctco_unnamed","call_id":"unnamed-call",
                "content":[]}
        ]),
        Value::Null,
    );
    let original = value(&req);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let transformed = value(&req);
    let mut expected = original["input"][0].clone();
    expected.as_object_mut().unwrap().remove("id");
    expected["tool_type"] = json!("function");
    assert_eq!(transformed["input"][0], expected);
    for index in 1..3 {
        assert_eq!(transformed["input"][index], original["input"][index]);
    }
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    assert_eq!(value(&req), transformed);
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

#[test]
fn unnamed_headers_pass_through_without_conversions_matching_the_response_rule() {
    let mut conversions = CustomToolConversions::default();
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    apply_request(&mut req, &cfg(&["*"]), &mut conversions).unwrap();
    for (response_cfg, ownership) in [
        (cfg(&["*"]), CustomToolConversions::default()),
        (cfg(&["apply_patch"]), conversions),
    ] {
        let mut state = StreamState::default();
        let events = vec![
            start(7, &call("", "a", "", None)),
            delta(7, r#"{"input":"native JSON"}"#),
            done(7, call("raw", "a", r#"{"input":"native JSON"}"#, None)),
        ];
        for event in events {
            let expected = value(&event);
            let emitted = stream(event, &response_cfg, &ownership, &mut state);
            assert_eq!(value(&emitted), json!([expected]));
            assert!(state.pending.is_empty());
        }
    }
}

fn pending_metadata_events(index: u32, call_id: &str, arguments: &str) -> Vec<UrpStreamEvent> {
    let mut start_event = start(index, &call("", call_id, "", None));
    if let UrpStreamEvent::NodeStart {
        header, extra_body, ..
    } = &mut start_event
    {
        extra_body.insert("start_metadata".to_string(), json!({"trace": 8}));
        if let NodeHeader::ToolCall { signature, id, .. } = header {
            *signature = Some(json!("signature-from-start"));
            *id = Some("item-from-start".to_string());
        }
    }
    let mut fragment = delta(index, arguments);
    if let UrpStreamEvent::NodeDelta {
        usage, extra_body, ..
    } = &mut fragment
    {
        *usage = Some(
            serde_json::from_value(json!({
                "input_tokens": 12, "output_tokens": 3, "provider_usage": "preserved",
            }))
            .unwrap(),
        );
        extra_body.insert("delta_metadata".to_string(), json!(["trace", 9]));
    }
    vec![start_event, fragment]
}

#[test]
fn owned_pending_completion_preserves_start_and_delta_metadata_for_both_terminal_paths() {
    let cfg = cfg(&["*"]);
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    for response_terminal in [false, true] {
        let mut state = StreamState::default();
        let pending = pending_metadata_events(19, "a", &wrap_input("完整 🦀"));
        let original_start = value(&pending[0]);
        let original_delta = value(&pending[1]);
        for event in pending {
            assert!(stream(event, &cfg, &conversions, &mut state).is_empty());
        }
        let completed = call("raw", "a", "", None);
        let terminal = if response_terminal {
            response_done(vec![completed])
        } else {
            done(19, completed)
        };
        let emitted = stream(terminal, &cfg, &conversions, &mut state);
        assert_eq!(emitted.len(), if response_terminal { 5 } else { 4 });
        let output = value(&emitted);
        assert_eq!(output[0]["node_index"], 19);
        assert_eq!(output[0]["header"]["tool_type"], "custom");
        assert_eq!(output[0]["header"]["name"], "raw");
        assert_eq!(
            output[0]["header"]["signature"],
            original_start["header"]["signature"]
        );
        assert_eq!(output[0]["header"]["id"], original_start["header"]["id"]);
        assert_eq!(
            output[0]["start_metadata"],
            original_start["start_metadata"]
        );
        assert_eq!(output[1]["node_index"], 19);
        assert_eq!(output[1]["delta"]["arguments"], "");
        assert_eq!(output[1]["usage"], original_delta["usage"]);
        assert_eq!(
            output[1]["delta_metadata"],
            original_delta["delta_metadata"]
        );
        assert_eq!(output[2]["node_index"], 19);
        assert_eq!(output[2]["delta"]["arguments"], "完整 🦀");
        assert_eq!(output[3]["node_index"], 19);
        assert_eq!(output[3]["node"]["arguments"], "完整 🦀");
        assert_eq!(output[3]["node"]["tool_type"], "custom");
        if response_terminal {
            assert_eq!(output[4]["output"][0]["arguments"], "完整 🦀");
        }
        assert!(state.pending.is_empty());
        assert!(state.calls.is_empty());
    }
}

#[test]
fn response_done_replays_pending_native_events_at_the_original_node_index() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([custom("raw"), function("read")]),
        json!([]),
        Value::Null,
    );
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let native_arguments = r#"{"input":"native JSON must remain wrapped"}"#;
    let pending = pending_metadata_events(23, "native", native_arguments);
    let expected = value(&pending);
    let mut state = StreamState::default();
    for event in pending {
        assert!(stream(event, &cfg, &conversions, &mut state).is_empty());
    }
    let native = call("read", "native", native_arguments, None);
    let emitted = stream(
        response_done(vec![native.clone()]),
        &cfg,
        &conversions,
        &mut state,
    );
    assert_eq!(emitted.len(), 4);
    assert_eq!(value(&emitted[0]), expected[0]);
    assert_eq!(value(&emitted[1]), expected[1]);
    assert_eq!(value(&emitted[2])["node_index"], 23);
    assert_eq!(value(&emitted[2])["node"], value(&native));
    assert_eq!(value(&emitted[3])["output"][0], value(&native));
    assert!(state.pending.is_empty());
}

#[test]
fn response_done_correlates_multiple_pending_calls_by_call_id() {
    let cfg = cfg(&["*"]);
    let mut req = request(
        json!([custom("raw"), function("read")]),
        json!([]),
        Value::Null,
    );
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let mut state = StreamState::default();
    for event in [
        start(8, &call("", "converted", "", None)),
        start(2, &call("", "native", "", None)),
        delta(8, &wrap_input("custom data")),
        delta(2, "{\"native\":true}"),
    ] {
        assert!(stream(event, &cfg, &conversions, &mut state).is_empty());
    }
    let output = stream(
        response_done(vec![
            call("read", "native", "{\"native\":true}", None),
            call("raw", "converted", &wrap_input("custom data"), None),
        ]),
        &cfg,
        &conversions,
        &mut state,
    );
    let output = value(&output);
    assert_eq!(output[0]["node_index"], 2);
    assert_eq!(output[1]["node_index"], 2);
    assert_eq!(output[2]["node_index"], 2);
    assert_eq!(output[3]["node_index"], 8);
    assert_eq!(output[4]["node_index"], 8);
    assert_eq!(output[4]["delta"]["arguments"], "custom data");
    assert_eq!(output[5]["node_index"], 8);
    assert!(state.pending.is_empty());
}

#[test]
fn stream_error_discards_pending_headers_deltas_usage_and_extensions() {
    let cfg = cfg(&["*"]);
    let mut req = request(json!([custom("raw")]), json!([]), Value::Null);
    let mut conversions = CustomToolConversions::default();
    apply_request(&mut req, &cfg, &mut conversions).unwrap();
    let mut state = StreamState::default();
    for event in pending_metadata_events(4, "a", "{\"input\":\"unfinished") {
        assert!(stream(event, &cfg, &conversions, &mut state).is_empty());
    }
    stream(
        start(5, &call("raw", "b", "", None)),
        &cfg,
        &conversions,
        &mut state,
    );
    assert!(!state.pending.is_empty());
    assert!(!state.calls.is_empty());
    let error = UrpStreamEvent::Error {
        code: Some("upstream_failed".to_string()),
        message: "failed".to_string(),
        extra_body: HashMap::from([("error_metadata".to_string(), json!(7))]),
    };
    let expected = value(&error);
    assert_eq!(
        value(&stream(error, &cfg, &conversions, &mut state)),
        json!([expected])
    );
    assert!(state.pending.is_empty());
    assert!(state.calls.is_empty());
    assert!(
        stream(
            response_done(vec![call("raw", "a", &wrap_input("later"), None)]),
            &cfg,
            &conversions,
            &mut state
        )
        .is_empty()
    );
}
