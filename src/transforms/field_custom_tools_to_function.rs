use crate::transforms::{
    Phase, Transform, TransformConfig, TransformEntry, TransformError, TransformRuntimeContext,
    TransformScope, TransformState, UrpData,
};
use crate::urp::{
    FunctionDefinition, Node, NodeDelta, NodeHeader, ToolCallType, ToolChoice, ToolDefinition,
    UrpRequest, UrpResponse, UrpStreamEvent,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};

const DEFAULT_NAMES: &[&str] = &["apply_patch"];
const INPUT_KEYS: &[&str] = &["input", "patch", "command", "content"];
const APPLY_PATCH_USAGE: &str = "Existing files: `*** Update File: path` with `@@` hunks. Unchanged hunk lines start with one space. `-` deletes. `+` inserts. Do not copy an unchanged line as both `-` and `+`. A hunk whose `-` lines and `+` lines are identical is a no-op. Do not rewrite an existing file as `*** Add File`. Example insert:\n*** Begin Patch\n*** Update File: path/file.py\n@@\n class Terminal:\n     host: str\n+    password: str | None = None\n*** End Patch";

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    names: Option<Vec<String>>,
}

struct Config {
    names: HashSet<String>,
    convert_all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ToolKey {
    namespace: Option<String>,
    name: String,
}

impl ToolKey {
    fn new(namespace: Option<&str>, name: &str) -> Self {
        Self {
            namespace: namespace.map(str::to_string),
            name: name.to_string(),
        }
    }
}

#[derive(Debug, Default)]
pub struct CustomToolConversions {
    converted: HashSet<ToolKey>,
    known: HashSet<ToolKey>,
    native: HashSet<ToolKey>,
    request_calls: HashMap<String, ToolKey>,
}

impl CustomToolConversions {
    fn resolve(&self, namespace: Option<&str>, name: &str, cfg: &Config) -> Option<ToolKey> {
        if !should_convert(cfg, name) {
            return None;
        }
        let key = ToolKey::new(namespace, name);
        if namespace.is_some() {
            return self.converted.contains(&key).then_some(key);
        }
        let mut candidates = self.known.iter().filter(|key| key.name == name);
        let candidate = candidates.next()?;
        if candidates.next().is_none() && self.converted.contains(candidate) {
            Some(candidate.clone())
        } else {
            None
        }
    }
}

struct BufferedCall {
    header: NodeHeader,
    arguments: String,
}

#[derive(Default)]
struct StreamState {
    replacement: Option<Vec<UrpStreamEvent>>,
    calls: BTreeMap<u32, BufferedCall>,
    restored_calls: HashMap<String, ToolKey>,
    completed: HashMap<String, Node>,
    used_indices: HashSet<u32>,
    failed: bool,
    pending: BTreeMap<u32, Vec<UrpStreamEvent>>,
}

impl TransformState for StreamState {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn finalize_stream_event(&mut self, event: UrpStreamEvent) -> Vec<UrpStreamEvent> {
        self.replacement.take().unwrap_or_else(|| vec![event])
    }
}

impl TransformConfig for Config {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn parse_names(raw: Option<Vec<String>>) -> Result<Config, TransformError> {
    let names = match raw {
        None => DEFAULT_NAMES
            .iter()
            .map(|n| (*n).to_string())
            .collect::<Vec<_>>(),
        Some(list) => list,
    };
    let mut set = HashSet::new();
    let mut convert_all = false;
    for name in names {
        if name.is_empty() {
            return Err(TransformError::InvalidConfig(
                "names entries must be non-empty strings".to_string(),
            ));
        }
        if name == "*" {
            convert_all = true;
            continue;
        }
        set.insert(name);
    }
    Ok(Config {
        names: set,
        convert_all,
    })
}

fn custom_name(tool: &ToolDefinition) -> Option<&str> {
    if tool.tool_type != "custom" {
        return None;
    }
    tool.custom
        .as_ref()
        .map(|c| c.name.as_str())
        .or(tool.name.as_deref())
}

fn should_convert(cfg: &Config, name: &str) -> bool {
    cfg.convert_all || cfg.names.contains(name)
}

fn wrap_input(raw: &str) -> String {
    json!({ "input": raw }).to_string()
}

fn unwrap_input(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Value::Object(map) = value {
            for key in INPUT_KEYS {
                if let Some(Value::String(s)) = map.get(*key) {
                    if *key == "input" || s.contains("Begin Patch") {
                        return s.clone();
                    }
                }
            }
        } else if let Value::String(s) = value {
            return s;
        }
    }
    raw.to_string()
}

fn strip_xml_invoke(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("<invoke") else {
        return raw.to_string();
    };
    let Some(gt) = rest.find('>') else {
        return raw.to_string();
    };
    let inner = &rest[gt + 1..];
    if let Some(end) = inner.rfind("</invoke>") {
        inner[..end].trim().to_string()
    } else {
        inner.trim().to_string()
    }
}

fn normalize_apply_patch(raw: &str) -> String {
    let stripped = strip_xml_invoke(raw);
    let mut out = String::new();
    let mut in_add_file = false;
    for line in stripped.lines() {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("*** End of File ***") || trimmed == "*** End of File" {
            in_add_file = false;
            continue;
        }
        if trimmed.starts_with("*** Begin Patch") {
            out.push_str("*** Begin Patch\n");
            in_add_file = false;
            continue;
        }
        if trimmed.starts_with("*** End Patch") {
            out.push_str("*** End Patch\n");
            in_add_file = false;
            continue;
        }
        if trimmed.starts_with("*** Add File") {
            out.push_str(line);
            out.push('\n');
            in_add_file = true;
            continue;
        }
        if trimmed.starts_with("***") {
            out.push_str(line);
            out.push('\n');
            in_add_file = false;
            continue;
        }
        if trimmed.starts_with("@@") {
            in_add_file = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_add_file && should_prefix_add_file_line(line) {
            out.push('+');
        }
        out.push_str(line);
        out.push('\n');
    }
    drop_noop_update_hunks(&out)
}

fn should_prefix_add_file_line(line: &str) -> bool {
    let t = line.trim_start();
    !(t.starts_with('+') || t.starts_with('-') || t.starts_with('\\') || t.starts_with("@@"))
}

fn apply_patch_tool_description(name: &str, existing: Option<String>) -> Option<String> {
    if name != "apply_patch" {
        return existing;
    }
    match existing {
        Some(text) if text.contains("identical is a no-op") => Some(text),
        Some(text) => Some(format!("{text}\n\n{APPLY_PATCH_USAGE}")),
        None => Some(APPLY_PATCH_USAGE.to_string()),
    }
}

fn drop_noop_update_hunks(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().starts_with("*** Update File") {
            let header = lines[i];
            i += 1;
            let start = i;
            while i < lines.len() {
                let t = lines[i].trim();
                if t.starts_with("***") && !t.starts_with("*** Update File") {
                    break;
                }
                i += 1;
            }
            if let Some(body) = rewrite_update_body(&lines[start..i]) {
                out.push_str(header);
                out.push('\n');
                out.push_str(&body);
            }
            continue;
        }
        out.push_str(lines[i]);
        out.push('\n');
        i += 1;
    }
    out
}

fn rewrite_update_body(body: &[&str]) -> Option<String> {
    let mut kept = String::new();
    let mut hunk: Vec<&str> = Vec::new();
    let mut any = false;
    for line in body {
        if line.trim().starts_with("@@") {
            if flush_update_hunk(&mut kept, &hunk) {
                any = true;
            }
            hunk.clear();
        }
        hunk.push(*line);
    }
    if flush_update_hunk(&mut kept, &hunk) {
        any = true;
    }
    any.then_some(kept)
}

fn flush_update_hunk(out: &mut String, hunk: &[&str]) -> bool {
    if hunk.is_empty() || hunk_is_noop(hunk) {
        return false;
    }
    for line in hunk {
        out.push_str(line);
        out.push('\n');
    }
    true
}

fn hunk_is_noop(hunk: &[&str]) -> bool {
    let mut minus: Vec<&str> = Vec::new();
    let mut plus: Vec<&str> = Vec::new();
    let mut has_change_op = false;
    for line in hunk {
        if line.trim().starts_with("@@") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            plus.push(rest);
            has_change_op = true;
        } else if let Some(rest) = line.strip_prefix('-') {
            minus.push(rest);
            has_change_op = true;
        } else {
            return false;
        }
    }
    has_change_op && minus == plus
}

fn function_parameters(name: &str) -> Value {
    let description = if name == "apply_patch" {
        "The entire apply_patch document. First line must be exactly `*** Begin Patch`. Last line must be exactly `*** End Patch`. New files use `*** Add File: path` and each content line starts with `+`. Existing files use `*** Update File: path` with `@@` hunks: unchanged lines start with one space, `-` deletes, `+` inserts. Do not copy an unchanged line as both `-` and `+`. Do not rewrite an existing file as Add File."
    } else {
        "The complete input string for this custom tool."
    };
    json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": description
            }
        },
        "required": ["input"]
    })
}

fn collect_identities(
    tools: &[ToolDefinition],
    inherited_namespace: Option<&str>,
    conversions: &mut CustomToolConversions,
) {
    for tool in tools {
        let namespace = tool.namespace.as_deref().or(inherited_namespace);
        if tool.tool_type == "namespace" {
            if let Some(children) = &tool.tools {
                collect_identities(children, tool.name.as_deref().or(namespace), conversions);
            }
        } else if let Some(name) = custom_name(tool) {
            conversions.known.insert(ToolKey::new(namespace, name));
        } else if let Some(function) = &tool.function {
            let key = ToolKey::new(namespace, &function.name);
            conversions.known.insert(key.clone());
            if !conversions.converted.contains(&key) {
                conversions.native.insert(key);
            }
        }
    }
}

fn collect_selected_custom_keys(
    tools: &[ToolDefinition],
    inherited_namespace: Option<&str>,
    cfg: &Config,
    keys: &mut HashSet<ToolKey>,
) {
    for tool in tools {
        let namespace = tool.namespace.as_deref().or(inherited_namespace);
        if tool.tool_type == "namespace" {
            if let Some(children) = &tool.tools {
                collect_selected_custom_keys(
                    children,
                    tool.name.as_deref().or(namespace),
                    cfg,
                    keys,
                );
            }
        } else if let Some(name) = custom_name(tool)
            && should_convert(cfg, name)
        {
            keys.insert(ToolKey::new(namespace, name));
        }
    }
}

fn convert_tool_to_function(
    tool: &mut ToolDefinition,
    inherited_namespace: Option<&str>,
    cfg: &Config,
    conversions: &mut CustomToolConversions,
) {
    let namespace = tool.namespace.as_deref().or(inherited_namespace);
    if tool.tool_type == "namespace" {
        if let Some(children) = tool.tools.as_mut() {
            let namespace = tool.name.as_deref().or(namespace);
            for child in children {
                convert_tool_to_function(child, namespace, cfg, conversions);
            }
        }
        return;
    }
    let Some(name) = custom_name(tool).map(str::to_string) else {
        return;
    };
    if !should_convert(cfg, &name) {
        return;
    }
    conversions.converted.insert(ToolKey::new(namespace, &name));
    let custom = tool.custom.take();
    let (description, extra_body) = custom
        .map(|custom| (custom.description, custom.extra_body))
        .unwrap_or_default();
    let description = apply_patch_tool_description(name.as_str(), description);
    tool.tool_type = "function".to_string();
    tool.name = Some(name.clone());
    tool.function = Some(FunctionDefinition {
        response_schema: None,
        parameters: Some(function_parameters(&name)),
        name,
        description,
        strict: Some(false),
        extra_body,
    });
}

fn convert_node_request(node: &mut Node, cfg: &Config, conversions: &mut CustomToolConversions) {
    let Node::ToolCall {
        tool_type,
        namespace,
        call_id,
        name,
        arguments,
        ..
    } = node
    else {
        return;
    };
    if *tool_type != ToolCallType::Custom || !should_convert(cfg, name) {
        return;
    }
    let key = ToolKey::new(namespace.as_deref(), name);
    conversions.converted.insert(key.clone());
    conversions.request_calls.insert(call_id.clone(), key);
    *tool_type = ToolCallType::Function;
    *arguments = wrap_input(arguments);
}

fn convert_node_response(
    node: &mut Node,
    cfg: &Config,
    conversions: &CustomToolConversions,
    calls: &mut HashMap<String, ToolKey>,
) -> bool {
    let Node::ToolCall {
        tool_type,
        namespace,
        call_id,
        name,
        arguments,
        ..
    } = node
    else {
        return false;
    };
    if *tool_type != ToolCallType::Function {
        return false;
    }
    let Some(key) = calls
        .get(call_id)
        .filter(|key| {
            key.name == *name
                && namespace
                    .as_ref()
                    .is_none_or(|namespace| Some(namespace) == key.namespace.as_ref())
        })
        .cloned()
        .or_else(|| conversions.resolve(namespace.as_deref(), name, cfg))
    else {
        return false;
    };
    *namespace = key.namespace.clone();
    calls.insert(call_id.clone(), key);
    *tool_type = ToolCallType::Custom;
    *arguments = unwrap_input(arguments);
    if name == "apply_patch" {
        *arguments = normalize_apply_patch(arguments);
    }
    true
}

fn convert_header_response(
    header: &mut NodeHeader,
    cfg: &Config,
    conversions: &CustomToolConversions,
    calls: &mut HashMap<String, ToolKey>,
) -> bool {
    let NodeHeader::ToolCall {
        tool_type,
        namespace,
        call_id,
        name,
        ..
    } = header
    else {
        return false;
    };
    if *tool_type != ToolCallType::Function {
        return false;
    }
    let Some(key) = conversions.resolve(namespace.as_deref(), name, cfg) else {
        return false;
    };
    *namespace = key.namespace.clone();
    calls.insert(call_id.clone(), key);
    *tool_type = ToolCallType::Custom;
    true
}

fn rewrite_tool_choice_request(value: &mut Value, converted: &HashSet<ToolKey>) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    match obj.get("type").and_then(Value::as_str) {
        Some("custom") => {
            let nested = obj.get("custom").and_then(Value::as_object);
            let name = nested
                .and_then(|custom| custom.get("name"))
                .or_else(|| obj.get("name"))
                .and_then(Value::as_str);
            let namespace = obj.get("namespace").and_then(Value::as_str).or_else(|| {
                nested
                    .and_then(|custom| custom.get("namespace"))
                    .and_then(Value::as_str)
            });
            if name.is_some_and(|name| converted.contains(&ToolKey::new(namespace, name))) {
                obj.insert("type".to_string(), json!("function"));
                if let Some(custom) = obj.remove("custom") {
                    obj.insert("function".to_string(), custom);
                }
            }
        }
        Some("allowed_tools") => {
            if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
                for tool in tools {
                    rewrite_tool_choice_request(tool, converted);
                }
            }
            if let Some(tools) = obj
                .get_mut("allowed_tools")
                .and_then(Value::as_object_mut)
                .and_then(|allowed| allowed.get_mut("tools"))
                .and_then(Value::as_array_mut)
            {
                for tool in tools {
                    rewrite_tool_choice_request(tool, converted);
                }
            }
        }
        _ => {}
    }
}

fn apply_request(
    req: &mut UrpRequest,
    cfg: &Config,
    conversions: &mut CustomToolConversions,
) -> Result<(), TransformError> {
    if let Some(tools) = &req.tools {
        collect_identities(tools, None, conversions);
    }
    for node in &req.input {
        if let Node::ToolCall {
            namespace,
            name,
            tool_type,
            ..
        } = node
        {
            let key = ToolKey::new(namespace.as_deref(), name);
            conversions.known.insert(key.clone());
            if *tool_type == ToolCallType::Function && !conversions.converted.contains(&key) {
                conversions.native.insert(key);
            }
        }
    }
    let mut candidates = HashSet::new();
    if let Some(tools) = &req.tools {
        collect_selected_custom_keys(tools, None, cfg, &mut candidates);
    }
    for node in &req.input {
        if let Node::ToolCall {
            namespace,
            name,
            tool_type: ToolCallType::Custom,
            ..
        } = node
            && should_convert(cfg, name)
        {
            candidates.insert(ToolKey::new(namespace.as_deref(), name));
        }
    }
    if let Some(key) = candidates.intersection(&conversions.native).next() {
        return Err(TransformError::Apply(format!(
            "custom tool conversion conflicts with native function identity: {:?}/{}",
            key.namespace, key.name,
        )));
    }
    let mut descriptor_keys = CustomToolConversions::default();
    if let Some(tools) = req.tools.as_mut() {
        for tool in tools {
            convert_tool_to_function(tool, None, cfg, &mut descriptor_keys);
        }
    }
    conversions
        .converted
        .extend(descriptor_keys.converted.iter().cloned());
    for node in &req.input {
        if let Node::ToolCall {
            tool_type: ToolCallType::Custom,
            namespace,
            call_id,
            name,
            ..
        } = node
            && should_convert(cfg, name)
        {
            let key = ToolKey::new(namespace.as_deref(), name);
            conversions.converted.insert(key.clone());
            conversions.request_calls.insert(call_id.clone(), key);
        }
    }
    if let Some(key) = conversions
        .converted
        .intersection(&conversions.native)
        .next()
    {
        return Err(TransformError::Apply(format!(
            "custom tool conversion conflicts with native function identity: {:?}/{}",
            key.namespace, key.name,
        )));
    }
    for node in &mut req.input {
        convert_node_request(node, cfg, conversions);
    }
    for node in &mut req.input {
        if let Node::ToolResult {
            tool_type,
            namespace,
            name,
            call_id,
            ..
        } = node
            && *tool_type == ToolCallType::Custom
            && (conversions.request_calls.contains_key(call_id)
                || name.as_deref().is_some_and(|name| {
                    descriptor_keys
                        .converted
                        .contains(&ToolKey::new(namespace.as_deref(), name))
                }))
        {
            *tool_type = ToolCallType::Function;
        }
    }
    if let Some(ToolChoice::Specific(value)) = req.tool_choice.as_mut() {
        rewrite_tool_choice_request(value, &descriptor_keys.converted);
    }
    Ok(())
}

fn convert_result_response(
    node: &mut Node,
    cfg: &Config,
    conversions: &CustomToolConversions,
    calls: &HashMap<String, ToolKey>,
) {
    let Node::ToolResult {
        tool_type,
        namespace,
        name,
        call_id,
        ..
    } = node
    else {
        return;
    };
    if *tool_type != ToolCallType::Function {
        return;
    }
    let key = calls.get(call_id).cloned().or_else(|| {
        name.as_deref()
            .and_then(|name| conversions.resolve(namespace.as_deref(), name, cfg))
    });
    if let Some(key) = key {
        *tool_type = ToolCallType::Custom;
        *namespace = key.namespace;
    }
}

fn convert_result_header_response(
    header: &mut NodeHeader,
    cfg: &Config,
    conversions: &CustomToolConversions,
    calls: &HashMap<String, ToolKey>,
) {
    let NodeHeader::ToolResult {
        tool_type,
        namespace,
        name,
        call_id,
        ..
    } = header
    else {
        return;
    };
    if *tool_type != ToolCallType::Function {
        return;
    }
    let key = calls.get(call_id).cloned().or_else(|| {
        name.as_deref()
            .and_then(|name| conversions.resolve(namespace.as_deref(), name, cfg))
    });
    if let Some(key) = key {
        *tool_type = ToolCallType::Custom;
        *namespace = key.namespace;
    }
}

fn apply_response(resp: &mut UrpResponse, cfg: &Config, conversions: &CustomToolConversions) {
    let mut calls = HashMap::new();
    for node in &mut resp.output {
        convert_node_response(node, cfg, conversions, &mut calls);
    }
    for node in &mut resp.output {
        convert_result_response(node, cfg, conversions, &calls);
    }
}

fn tool_header(node: &Node) -> Option<NodeHeader> {
    let Node::ToolCall {
        namespace,
        signature,
        id,
        tool_type,
        call_id,
        name,
        ..
    } = node
    else {
        return None;
    };
    Some(NodeHeader::ToolCall {
        namespace: namespace.clone(),
        signature: signature.clone(),
        id: id.clone(),
        tool_type: *tool_type,
        call_id: call_id.clone(),
        name: name.clone(),
    })
}

fn completion_events(node_index: u32, node: &Node, include_start: bool) -> Vec<UrpStreamEvent> {
    let Node::ToolCall { arguments, .. } = node else {
        return Vec::new();
    };
    let mut events = Vec::new();
    if include_start && let Some(header) = tool_header(node) {
        events.push(UrpStreamEvent::NodeStart {
            node_index,
            header,
            extra_body: HashMap::new(),
        });
    }
    events.push(UrpStreamEvent::NodeDelta {
        node_index,
        delta: NodeDelta::ToolCallArguments {
            arguments: arguments.clone(),
        },
        usage: None,
        extra_body: HashMap::new(),
    });
    events
}

fn reuse_completed_arguments(node: &mut Node, completed: &HashMap<String, Node>) -> bool {
    let Node::ToolCall {
        namespace,
        tool_type,
        call_id,
        name,
        arguments,
        ..
    } = node
    else {
        return false;
    };
    let Some(Node::ToolCall {
        namespace: original_namespace,
        name: original_name,
        arguments: original_arguments,
        ..
    }) = completed.get(call_id)
    else {
        return false;
    };
    if name != original_name {
        return false;
    }
    *namespace = original_namespace.clone();
    *tool_type = ToolCallType::Custom;
    *arguments = original_arguments.clone();
    true
}

fn supply_buffered_arguments(node: &mut Node, buffered: &BufferedCall) {
    if let Node::ToolCall { arguments, .. } = node
        && arguments.is_empty()
        && !buffered.arguments.is_empty()
    {
        *arguments = buffered.arguments.clone();
    }
}

fn pending_call_id(events: &[UrpStreamEvent]) -> Option<&str> {
    events.iter().find_map(|event| match event {
        UrpStreamEvent::NodeStart {
            header: NodeHeader::ToolCall { call_id, .. },
            ..
        } => Some(call_id.as_str()),
        _ => None,
    })
}

fn supply_pending_arguments(node: &mut Node, events: &[UrpStreamEvent]) {
    if let Node::ToolCall { arguments, .. } = node
        && arguments.is_empty()
    {
        for event in events {
            if let UrpStreamEvent::NodeDelta {
                delta:
                    NodeDelta::ToolCallArguments {
                        arguments: fragment,
                    },
                ..
            } = event
            {
                arguments.push_str(fragment);
            }
        }
    }
}

fn restored_completion_events(
    node_index: u32,
    node: &Node,
    pending: Option<Vec<UrpStreamEvent>>,
    include_start: bool,
) -> Vec<UrpStreamEvent> {
    let Some(pending) = pending else {
        return completion_events(node_index, node, include_start);
    };
    let mut events = Vec::new();
    for mut event in pending {
        match &mut event {
            UrpStreamEvent::NodeStart {
                header:
                    NodeHeader::ToolCall {
                        namespace,
                        tool_type,
                        name,
                        ..
                    },
                ..
            } => {
                if let Node::ToolCall {
                    namespace: restored_namespace,
                    tool_type: restored_type,
                    name: restored_name,
                    ..
                } = node
                {
                    *namespace = restored_namespace.clone();
                    *tool_type = *restored_type;
                    *name = restored_name.clone();
                }
                events.push(event);
            }
            UrpStreamEvent::NodeDelta {
                delta: NodeDelta::ToolCallArguments { arguments },
                usage,
                extra_body,
                ..
            } => {
                arguments.clear();
                if usage.is_some() || !extra_body.is_empty() {
                    events.push(event);
                }
            }
            _ => events.push(event),
        }
    }
    events.extend(completion_events(node_index, node, false));
    events
}

fn apply_stream(
    event: &mut UrpStreamEvent,
    cfg: &Config,
    conversions: &CustomToolConversions,
    state: &mut StreamState,
) {
    if state.failed {
        state.replacement = Some(Vec::new());
        return;
    }
    if let UrpStreamEvent::NodeStart { node_index, .. }
    | UrpStreamEvent::NodeDelta { node_index, .. }
    | UrpStreamEvent::NodeDone { node_index, .. } = event
    {
        state.used_indices.insert(*node_index);
    }
    let mut before = Vec::new();
    match event {
        UrpStreamEvent::NodeStart {
            node_index, header, ..
        } => {
            state.used_indices.insert(*node_index);
            let unnamed_function = matches!(header, NodeHeader::ToolCall {
                tool_type: ToolCallType::Function,
                name,
                ..
            } if name.is_empty());
            if unnamed_function
                && conversions
                    .converted
                    .iter()
                    .any(|key| should_convert(cfg, &key.name))
            {
                state.pending.insert(*node_index, vec![event.clone()]);
                state.replacement = Some(Vec::new());
                return;
            }
            if convert_header_response(header, cfg, conversions, &mut state.restored_calls) {
                state.calls.insert(
                    *node_index,
                    BufferedCall {
                        header: header.clone(),
                        arguments: String::new(),
                    },
                );
            }
            convert_result_header_response(header, cfg, conversions, &state.restored_calls);
        }
        UrpStreamEvent::NodeDelta {
            node_index,
            delta: NodeDelta::ToolCallArguments { arguments },
            usage,
            extra_body,
        } => {
            if let Some(pending) = state.pending.get_mut(node_index) {
                pending.push(event.clone());
                state.replacement = Some(Vec::new());
                return;
            }
            if let Some(buffered) = state.calls.get_mut(node_index) {
                buffered.arguments.push_str(arguments);
                arguments.clear();
                if usage.is_none() && extra_body.is_empty() {
                    state.replacement = Some(Vec::new());
                }
            }
        }
        UrpStreamEvent::NodeDone {
            node_index, node, ..
        } => {
            if !matches!(node, Node::ToolCall { .. }) {
                convert_result_response(node, cfg, conversions, &state.restored_calls);
                return;
            }
            let pending = state.pending.remove(node_index);
            let buffered = state.calls.remove(node_index);
            let mut candidate = node.clone();
            if let Some(buffered) = &buffered {
                supply_buffered_arguments(&mut candidate, buffered);
            }
            if let Some(pending) = &pending {
                supply_pending_arguments(&mut candidate, pending);
            }
            if convert_node_response(&mut candidate, cfg, conversions, &mut state.restored_calls) {
                *node = candidate;
                before = restored_completion_events(*node_index, node, pending, buffered.is_none());
                if let Node::ToolCall { call_id, .. } = node {
                    state.completed.insert(call_id.clone(), node.clone());
                }
                state.used_indices.insert(*node_index);
            } else if let Some(mut pending) = pending {
                before.append(&mut pending);
            }
            convert_result_response(node, cfg, conversions, &state.restored_calls);
        }
        UrpStreamEvent::ResponseDone { output, .. } => {
            for node in output.iter_mut() {
                if !matches!(node, Node::ToolCall { .. }) {
                    continue;
                }
                if reuse_completed_arguments(node, &state.completed) {
                    continue;
                }
                let node_index = if let Node::ToolCall { call_id, .. } = node {
                    state
                        .calls
                        .iter()
                        .find_map(|(index, buffered)| match &buffered.header {
                            NodeHeader::ToolCall {
                                call_id: candidate, ..
                            } if candidate == call_id => Some(*index),
                            _ => None,
                        })
                        .or_else(|| {
                            state.pending.iter().find_map(|(index, events)| {
                                (pending_call_id(events) == Some(call_id.as_str()))
                                    .then_some(*index)
                            })
                        })
                } else {
                    None
                };
                let pending = node_index.and_then(|index| state.pending.remove(&index));
                let buffered = node_index.and_then(|index| state.calls.remove(&index));
                let mut candidate = node.clone();
                if let Some(buffered) = &buffered {
                    supply_buffered_arguments(&mut candidate, buffered);
                }
                if let Some(pending) = &pending {
                    supply_pending_arguments(&mut candidate, pending);
                }
                if !convert_node_response(
                    &mut candidate,
                    cfg,
                    conversions,
                    &mut state.restored_calls,
                ) {
                    if let (Some(index), Some(pending)) = (node_index, pending) {
                        before.extend(pending);
                        before.push(UrpStreamEvent::NodeDone {
                            node_index: index,
                            node: node.clone(),
                            usage: None,
                            extra_body: HashMap::new(),
                        });
                    }
                    continue;
                }
                *node = candidate;
                let index = node_index.unwrap_or_else(|| {
                    (0..=u32::MAX)
                        .find(|index| !state.used_indices.contains(index))
                        .expect("stream node index available")
                });
                state.used_indices.insert(index);
                before.extend(restored_completion_events(
                    index,
                    node,
                    pending,
                    node_index.is_none(),
                ));
                before.push(UrpStreamEvent::NodeDone {
                    node_index: index,
                    node: node.clone(),
                    usage: None,
                    extra_body: HashMap::new(),
                });
                if let Node::ToolCall { call_id, .. } = node {
                    state.completed.insert(call_id.clone(), node.clone());
                }
            }
            for node in output {
                convert_result_response(node, cfg, conversions, &state.restored_calls);
            }
            state.calls.clear();
            state.pending.clear();
        }
        UrpStreamEvent::Error { .. } => {
            state.calls.clear();
            state.pending.clear();
            state.restored_calls.clear();
            state.failed = true;
        }
        _ => {}
    }
    if !before.is_empty() {
        before.push(event.clone());
        state.replacement = Some(before);
    }
}

pub struct FieldCustomToolsToFunctionTransform;

#[async_trait]
impl Transform for FieldCustomToolsToFunctionTransform {
    fn type_id(&self) -> &'static str {
        "field_custom_tools_to_function"
    }

    fn display_name(&self) -> crate::transforms::LocalizedText {
        &[
            ("en", "Field: custom tools to function"),
            ("zh", "字段：自定义工具转函数"),
        ]
    }

    fn display_description(&self) -> crate::transforms::LocalizedText {
        &[
            (
                "en",
                "Converts Codex freeform/custom tools such as apply_patch into JSON function tools for Grok/CPA-style upstreams, then restores custom tool calls on the response.",
            ),
            (
                "zh",
                "将 Codex 的 apply_patch 等 custom/freeform 工具转成 JSON function，供 Grok/CPA 上游使用，并在响应中恢复为 custom 工具调用。",
            ),
        ]
    }

    fn supported_phases(&self) -> &'static [Phase] {
        &[Phase::Request, Phase::Response]
    }

    fn supported_scopes(&self) -> &'static [TransformScope] {
        &[
            TransformScope::Provider,
            TransformScope::Global,
            TransformScope::ApiKey,
        ]
    }

    fn config_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "names": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 },
                    "default": ["apply_patch"],
                    "description": "Custom tool names to convert. Omit for apply_patch only. Include \"*\" to convert every custom tool."
                }
            },
            "additionalProperties": false
        })
    }

    fn parse_config(&self, raw: Value) -> Result<Box<dyn TransformConfig>, TransformError> {
        let raw_cfg: RawConfig = serde_json::from_value(raw)
            .map_err(|e| TransformError::InvalidConfig(e.to_string()))?;
        Ok(Box::new(parse_names(raw_cfg.names)?))
    }

    fn init_state(&self) -> Box<dyn TransformState> {
        Box::new(StreamState::default())
    }

    async fn apply(
        &self,
        data: UrpData<'_>,
        phase: Phase,
        context: &TransformRuntimeContext,
        config: &dyn TransformConfig,
        state: &mut dyn TransformState,
    ) -> Result<(), TransformError> {
        let cfg = config
            .as_any()
            .downcast_ref::<Config>()
            .ok_or_else(|| TransformError::InvalidConfig("internal config type mismatch".into()))?;
        if !cfg.convert_all && cfg.names.is_empty() {
            return Ok(());
        }
        let state = state
            .as_any_mut()
            .downcast_mut::<StreamState>()
            .ok_or_else(|| TransformError::Apply("internal state type mismatch".into()))?;
        let mut conversions = context
            .custom_tool_conversions
            .lock()
            .map_err(|_| TransformError::Apply("custom tool conversion state poisoned".into()))?;
        match (phase, data) {
            (Phase::Request, UrpData::Request(req)) => apply_request(req, cfg, &mut conversions)?,
            (Phase::Response, UrpData::Response(resp)) => apply_response(resp, cfg, &conversions),
            (Phase::Response, UrpData::Stream(event)) => {
                apply_stream(event, cfg, &conversions, state)
            }
            _ => {}
        }
        Ok(())
    }
}

inventory::submit!(TransformEntry {
    factory: || Box::new(FieldCustomToolsToFunctionTransform),
});

#[cfg(test)]
#[path = "field_custom_tools_to_function_tests.rs"]
mod tests;
