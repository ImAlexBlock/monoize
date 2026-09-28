use crate::transforms::{
    Phase, Transform, TransformConfig, TransformEntry, TransformError, TransformRuntimeContext,
    TransformScope, TransformState, UrpData,
};
use crate::urp::{ImageSource, Node, NodeDelta, NodeHeader, OrdinaryRole, UrpStreamEvent};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::any::Any;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct Config {}

impl TransformConfig for Config {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct ImageMarkdownToOutputTransform;

struct StreamTextNodeState {
    header_id: Option<String>,
    header_phase: Option<String>,
    header_signature: Option<Value>,
    header_citations: Vec<crate::urp::Citation>,
    header_extra_body: HashMap<String, Value>,
    buffered_events: Option<Vec<UrpStreamEvent>>,
    buffered_tail: String,
    cleaned_content: String,
    start_emitted: bool,
    saw_delta: bool,
}

struct StreamState {
    replacement: Option<Vec<UrpStreamEvent>>,
    node_text_parts: HashMap<u32, StreamTextNodeState>,
    next_synthetic_node_index: u32,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            replacement: None,
            node_text_parts: HashMap::new(),
            next_synthetic_node_index: u32::MAX,
        }
    }
}

impl TransformState for StreamState {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn finalize_stream_event(&mut self, event: UrpStreamEvent) -> Vec<UrpStreamEvent> {
        self.replacement.take().unwrap_or_else(|| vec![event])
    }
}

#[async_trait]
impl Transform for ImageMarkdownToOutputTransform {
    fn type_id(&self) -> &'static str {
        "image_markdown_to_output"
    }

    fn display_name(&self) -> crate::transforms::LocalizedText {
        &[
            ("en", "Image: Markdown to image nodes"),
            ("zh", "图像：Markdown 转图像节点"),
        ]
    }

    fn display_description(&self) -> crate::transforms::LocalizedText {
        &[
            (
                "en",
                "Extracts Markdown image blocks from assistant text and converts them into ordinary assistant image nodes. Inverse of image_output_to_markdown.",
            ),
            (
                "zh",
                "从 assistant 文本中提取 Markdown 图片语法并转换为标准的 assistant 图像节点。与 image_output_to_markdown 互逆。",
            ),
        ]
    }

    fn supported_phases(&self) -> &'static [Phase] {
        &[Phase::Response]
    }

    fn supported_scopes(&self) -> &'static [TransformScope] {
        &[TransformScope::Provider, TransformScope::ApiKey]
    }

    fn config_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn parse_config(&self, raw: Value) -> Result<Box<dyn TransformConfig>, TransformError> {
        let cfg: Config = serde_json::from_value(raw)
            .map_err(|e| TransformError::InvalidConfig(e.to_string()))?;
        Ok(Box::new(cfg))
    }

    fn init_state(&self) -> Box<dyn TransformState> {
        Box::new(StreamState::default())
    }

    async fn apply(
        &self,
        data: UrpData<'_>,
        _phase: Phase,
        _context: &TransformRuntimeContext,
        _config: &dyn TransformConfig,
        state: &mut dyn TransformState,
    ) -> Result<(), TransformError> {
        match data {
            UrpData::Response(resp) => {
                rewrite_assistant_markdown_images_nodes(&mut resp.output);
            }
            UrpData::Stream(event) => {
                let Some(stream_state) = state.as_any_mut().downcast_mut::<StreamState>() else {
                    return Err(TransformError::Apply("invalid stream state".to_string()));
                };
                apply_stream(event, stream_state);
            }
            UrpData::Request(_) => {}
        }
        Ok(())
    }
}

fn rewrite_assistant_markdown_images_nodes(nodes: &mut Vec<Node>) {
    let mut rewritten = Vec::with_capacity(nodes.len());
    for node in nodes.drain(..) {
        match node {
            Node::Text {
                logprobs,
                signature,
                mut citations,
                id,
                role: OrdinaryRole::Assistant,
                content,
                phase,
                extra_body,
            } => {
                let (cleaned, images, removed) = extract_markdown_images_with_ranges(&content);
                let changed = !images.is_empty();
                if changed {
                    remap_citations(&mut citations, &removed);
                }
                if !cleaned.is_empty() || !changed {
                    rewritten.push(Node::Text {
                        logprobs: if changed { None } else { logprobs },
                        signature: if changed { None } else { signature },
                        citations,
                        id,
                        role: OrdinaryRole::Assistant,
                        content: cleaned,
                        phase,
                        extra_body,
                    });
                }
                for source in images {
                    rewritten.push(Node::Image {
                        metadata: Default::default(),

                        id: None,
                        role: OrdinaryRole::Assistant,
                        source,
                        extra_body: HashMap::new(),
                    });
                }
            }
            other => rewritten.push(other),
        }
    }
    *nodes = rewritten;
}

#[cfg(test)]
fn extract_markdown_images_from_text(content: &str) -> (String, Vec<ImageSource>) {
    let (cleaned, images, _) = extract_markdown_images_with_ranges(content);
    (cleaned, images)
}

fn extract_markdown_images_with_ranges(
    content: &str,
) -> (
    String,
    Vec<ImageSource>,
    Vec<crate::urp::citations::TextRange>,
) {
    let (segments, tail) = split_stream_segments(content, true);
    debug_assert!(tail.is_empty());
    let mut images = Vec::new();
    let mut cleaned = String::new();
    let mut removed = Vec::new();
    let mut cursor = 0;
    for segment in segments {
        match segment {
            StreamSegment::Text(text) => {
                cursor += text.chars().count() as u64;
                cleaned.push_str(&text);
            }
            StreamSegment::Image {
                source,
                removed_chars,
            } => {
                removed.push(crate::urp::citations::TextRange {
                    start: cursor,
                    end: cursor + removed_chars,
                });
                cursor += removed_chars;
                images.push(source);
            }
        }
    }
    (cleaned, images, removed)
}

fn remap_citations(
    citations: &mut Vec<crate::urp::Citation>,
    removed: &[crate::urp::citations::TextRange],
) {
    citations.retain_mut(|citation| {
        let Some(range) = citation.answer_range.as_mut() else {
            return true;
        };
        if removed
            .iter()
            .any(|span| range.start < span.end && range.end > span.start)
        {
            return false;
        }
        let shift: u64 = removed
            .iter()
            .filter(|span| span.end <= range.start)
            .map(|span| span.end - span.start)
            .sum();
        range.start -= shift;
        range.end -= shift;
        true
    });
}

#[derive(Debug)]
enum StreamSegment {
    Text(String),
    Image {
        source: ImageSource,
        removed_chars: u64,
    },
}

enum CandidateParse {
    Valid { end: usize, source: ImageSource },
    Invalid { safe_end: usize },
    Incomplete,
}

fn split_stream_segments(content: &str, terminal: bool) -> (Vec<StreamSegment>, String) {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut text = String::new();
    let mut segments = Vec::new();

    while i < bytes.len() {
        if bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[') {
            match parse_markdown_candidate(content, i) {
                CandidateParse::Valid { end, source } => {
                    if !text.is_empty() {
                        segments.push(StreamSegment::Text(std::mem::take(&mut text)));
                    }
                    segments.push(StreamSegment::Image {
                        source,
                        removed_chars: content[i..end].chars().count() as u64,
                    });
                    i = end;
                    continue;
                }
                CandidateParse::Invalid { safe_end } => {
                    text.push_str(&content[i..safe_end]);
                    i = safe_end;
                    continue;
                }
                CandidateParse::Incomplete => break,
            }
        }

        let ch = content[i..].chars().next().expect("stream parser char");
        text.push(ch);
        i += ch.len_utf8();
    }

    let mut tail = if i < bytes.len() {
        content[i..].to_string()
    } else {
        String::new()
    };
    if terminal && !tail.is_empty() {
        text.push_str(&tail);
        tail.clear();
    }
    if !text.is_empty() {
        segments.push(StreamSegment::Text(text));
    }
    (segments, tail)
}

fn parse_markdown_candidate(content: &str, start: usize) -> CandidateParse {
    let bytes = content.as_bytes();
    let mut close_bracket = start + 2;
    while close_bracket < bytes.len() && bytes[close_bracket] != b']' {
        close_bracket += 1;
    }
    if close_bracket >= bytes.len() {
        return CandidateParse::Incomplete;
    }
    let after_bracket = close_bracket + 1;
    if after_bracket >= bytes.len() {
        return CandidateParse::Incomplete;
    }
    if bytes[after_bracket] != b'(' {
        return CandidateParse::Invalid {
            safe_end: after_bracket,
        };
    }

    let url_start = after_bracket + 1;
    if url_start >= bytes.len() {
        return CandidateParse::Incomplete;
    }

    let mut i = url_start;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b')' {
            if i == url_start {
                return CandidateParse::Invalid { safe_end: i + 1 };
            }
            let url = &content[url_start..i];
            return match parse_markdown_image_source(url) {
                Some(source) => CandidateParse::Valid { end: i + 1, source },
                None => CandidateParse::Invalid { safe_end: i + 1 },
            };
        }
        if byte.is_ascii_whitespace() {
            return CandidateParse::Invalid { safe_end: i + 1 };
        }
        i += 1;
    }
    CandidateParse::Incomplete
}

fn apply_stream(event: &mut UrpStreamEvent, state: &mut StreamState) {
    if matches!(
        event,
        UrpStreamEvent::NodeStart {
            header: NodeHeader::NextDownstreamEnvelopeExtra,
            ..
        } | UrpStreamEvent::NodeDone {
            node: Node::NextDownstreamEnvelopeExtra { .. },
            ..
        }
    ) {
        state.replacement = Some(vec![event.clone()]);
        return;
    }
    if apply_node_stream(event, state) {
        return;
    }
    if let UrpStreamEvent::ResponseDone { output, .. } = event {
        rewrite_assistant_markdown_images_nodes(output);
        state.node_text_parts.clear();
    }
}

fn apply_node_stream(event: &mut UrpStreamEvent, state: &mut StreamState) -> bool {
    match event {
        UrpStreamEvent::NodeStart {
            node_index,
            header:
                NodeHeader::Text {
                    signature,
                    citations,
                    id,
                    role: OrdinaryRole::Assistant,
                    phase,
                },
            extra_body,
        } => {
            state.node_text_parts.insert(
                *node_index,
                StreamTextNodeState {
                    header_id: id.clone(),
                    header_phase: phase.clone(),
                    header_signature: signature.clone(),
                    header_citations: citations.clone(),
                    header_extra_body: extra_body.clone(),
                    buffered_events: (signature.is_some() || !citations.is_empty()).then(Vec::new),
                    buffered_tail: String::new(),
                    cleaned_content: String::new(),
                    start_emitted: false,
                    saw_delta: false,
                },
            );
            state.replacement = Some(Vec::new());
            true
        }
        UrpStreamEvent::NodeDelta {
            node_index,
            delta:
                NodeDelta::Text {
                    logprobs,
                    signature,
                    citations,
                    content,
                },
            usage,
            extra_body,
        } => {
            let Some(mut node_state) = state.node_text_parts.remove(node_index) else {
                return false;
            };
            node_state.saw_delta = true;
            let mut emitted = Vec::new();
            // Later Markdown extraction can invalidate a signature or citation already sent.
            if node_state.buffered_events.is_some()
                || logprobs.is_some()
                || signature.is_some()
                || !citations.is_empty()
            {
                node_state
                    .buffered_events
                    .get_or_insert_with(Vec::new)
                    .push(UrpStreamEvent::NodeDelta {
                        node_index: *node_index,
                        delta: NodeDelta::Text {
                            logprobs: logprobs.clone(),
                            signature: signature.clone(),
                            citations: citations.clone(),
                            content: content.clone(),
                        },
                        usage: usage.clone(),
                        extra_body: extra_body.clone(),
                    });
            } else {
                emit_text_delta(
                    *node_index,
                    state,
                    &mut node_state,
                    content,
                    usage.clone(),
                    extra_body,
                    &mut emitted,
                );
            }
            state.node_text_parts.insert(*node_index, node_state);
            state.replacement = Some(emitted);
            true
        }
        UrpStreamEvent::NodeDone {
            node_index,
            node:
                Node::Text {
                    logprobs,
                    signature,
                    citations,
                    id,
                    role: OrdinaryRole::Assistant,
                    content,
                    phase,
                    extra_body: node_extra_body,
                },
            usage,
            extra_body: event_extra_body,
        } => {
            let mut node_state =
                state
                    .node_text_parts
                    .remove(node_index)
                    .unwrap_or_else(|| StreamTextNodeState {
                        header_id: id.clone(),
                        header_phase: phase.clone(),
                        header_signature: signature.clone(),
                        header_citations: citations.clone(),
                        header_extra_body: HashMap::new(),
                        buffered_events: None,
                        buffered_tail: String::new(),
                        cleaned_content: String::new(),
                        start_emitted: false,
                        saw_delta: false,
                    });
            let (cleaned_terminal, images, removed) = extract_markdown_images_with_ranges(content);
            let changed = !images.is_empty();
            let mut terminal_citations = citations.clone();
            if changed {
                node_state.header_signature = None;
                node_state.header_citations.clear();
                remap_citations(&mut terminal_citations, &removed);
            }
            let mut emitted = Vec::new();
            if let Some(buffered) = node_state.buffered_events.take() {
                if changed {
                    for event in buffered {
                        if let UrpStreamEvent::NodeDelta {
                            delta: NodeDelta::Text { content, .. },
                            usage,
                            extra_body,
                            ..
                        } = event
                        {
                            emit_text_delta(
                                *node_index,
                                state,
                                &mut node_state,
                                &content,
                                usage,
                                &extra_body,
                                &mut emitted,
                            );
                        }
                    }
                } else if !buffered.is_empty() {
                    ensure_text_node_start(*node_index, &mut node_state, &mut emitted);
                    let tail = std::mem::take(&mut node_state.buffered_tail);
                    emit_node_segments(
                        *node_index,
                        state,
                        &mut node_state,
                        vec![StreamSegment::Text(tail)],
                        &mut emitted,
                        &HashMap::new(),
                    );
                    emitted.extend(buffered);
                }
            }
            if !node_state.saw_delta {
                node_state.buffered_tail.push_str(content);
            }
            let (segments, tail) = split_stream_segments(&node_state.buffered_tail, true);
            node_state.buffered_tail = tail;
            emit_node_segments(
                *node_index,
                state,
                &mut node_state,
                segments,
                &mut emitted,
                &HashMap::new(),
            );
            if !changed {
                ensure_text_node_start(*node_index, &mut node_state, &mut emitted);
            }
            if node_state.start_emitted {
                emitted.push(UrpStreamEvent::NodeDone {
                    node_index: *node_index,
                    node: Node::Text {
                        logprobs: if changed { None } else { logprobs.clone() },
                        signature: if changed { None } else { signature.clone() },
                        citations: terminal_citations,
                        id: id.clone(),
                        role: OrdinaryRole::Assistant,
                        content: cleaned_terminal,
                        phase: phase.clone(),
                        extra_body: node_extra_body.clone(),
                    },
                    usage: None,
                    extra_body: event_extra_body.clone(),
                });
            }
            attach_usage_to_last_node_event(&mut emitted, usage.clone());
            state.replacement = Some(emitted);
            true
        }
        _ => false,
    }
}

fn allocate_synthetic_node_index(state: &mut StreamState) -> u32 {
    let node_index = state.next_synthetic_node_index;
    state.next_synthetic_node_index = state.next_synthetic_node_index.saturating_sub(1);
    node_index
}

fn ensure_text_node_start(
    node_index: u32,
    node_state: &mut StreamTextNodeState,
    emitted: &mut Vec<UrpStreamEvent>,
) {
    if node_state.start_emitted {
        return;
    }
    emitted.push(UrpStreamEvent::NodeStart {
        node_index,
        header: NodeHeader::Text {
            signature: node_state.header_signature.clone(),
            citations: node_state.header_citations.clone(),
            id: node_state.header_id.clone(),
            role: OrdinaryRole::Assistant,
            phase: node_state.header_phase.clone(),
        },
        extra_body: node_state.header_extra_body.clone(),
    });
    node_state.start_emitted = true;
}

fn emit_synthetic_image_node(
    state: &mut StreamState,
    source: ImageSource,
    emitted: &mut Vec<UrpStreamEvent>,
) {
    let node_index = allocate_synthetic_node_index(state);
    emitted.push(UrpStreamEvent::NodeStart {
        node_index,
        header: NodeHeader::Image {
            metadata: Default::default(),

            id: None,
            role: OrdinaryRole::Assistant,
        },
        extra_body: HashMap::new(),
    });
    emitted.push(UrpStreamEvent::NodeDone {
        node_index,
        node: Node::Image {
            metadata: Default::default(),

            id: None,
            role: OrdinaryRole::Assistant,
            source,
            extra_body: HashMap::new(),
        },
        usage: None,
        extra_body: HashMap::new(),
    });
}

fn emit_node_segments(
    node_index: u32,
    state: &mut StreamState,
    node_state: &mut StreamTextNodeState,
    segments: Vec<StreamSegment>,
    emitted: &mut Vec<UrpStreamEvent>,
    delta_extra_body: &HashMap<String, Value>,
) {
    for segment in segments {
        match segment {
            StreamSegment::Text(text) => {
                if text.is_empty() {
                    continue;
                }
                ensure_text_node_start(node_index, node_state, emitted);
                node_state.cleaned_content.push_str(&text);
                emitted.push(UrpStreamEvent::NodeDelta {
                    node_index,
                    delta: NodeDelta::Text {
                        logprobs: None,
                        signature: None,
                        citations: Vec::new(),
                        content: text,
                    },
                    usage: None,
                    extra_body: delta_extra_body.clone(),
                });
            }
            StreamSegment::Image { source, .. } => {
                emit_synthetic_image_node(state, source, emitted)
            }
        }
    }
}

fn emit_text_delta(
    node_index: u32,
    state: &mut StreamState,
    node_state: &mut StreamTextNodeState,
    content: &str,
    usage: Option<crate::urp::Usage>,
    extra_body: &HashMap<String, Value>,
    emitted: &mut Vec<UrpStreamEvent>,
) {
    let emitted_start = emitted.len();
    let combined = format!("{}{}", node_state.buffered_tail, content);
    let (segments, tail) = split_stream_segments(&combined, false);
    node_state.buffered_tail = tail;
    emit_node_segments(node_index, state, node_state, segments, emitted, extra_body);
    attach_usage_to_last_node_event(&mut emitted[emitted_start..], usage);
}

fn attach_usage_to_last_node_event(
    events: &mut [UrpStreamEvent],
    usage: Option<crate::urp::Usage>,
) {
    let Some(usage) = usage else {
        return;
    };
    let Some(last) = events.last_mut() else {
        return;
    };
    match last {
        UrpStreamEvent::NodeDelta { usage: slot, .. }
        | UrpStreamEvent::NodeDone { usage: slot, .. }
        | UrpStreamEvent::ResponseDone { usage: slot, .. } => *slot = Some(usage),
        _ => {}
    }
}

fn parse_markdown_image_source(url: &str) -> Option<ImageSource> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        if !meta.ends_with(";base64") {
            return None;
        }
        let media_type = meta.trim_end_matches(";base64");
        if !media_type.starts_with("image/") || data.is_empty() {
            return None;
        }
        return Some(ImageSource::Base64 {
            media_type: media_type.to_string(),
            data: data.to_string(),
        });
    }
    Some(ImageSource::Url {
        url: url.to_string(),
        detail: None,
    })
}

inventory::submit!(TransformEntry {
    factory: || Box::new(ImageMarkdownToOutputTransform),
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_url_and_base64_markdown_images() {
        let (cleaned, images) = extract_markdown_images_from_text(
            "hello ![a](https://example.com/a.png) world ![b](data:image/png;base64,QUJD)",
        );
        assert_eq!(cleaned, "hello  world ");
        assert_eq!(images.len(), 2);
        match &images[0] {
            ImageSource::Url { url, .. } => assert_eq!(url, "https://example.com/a.png"),
            _ => panic!("expected url image"),
        }
        match &images[1] {
            ImageSource::Base64 { media_type, data } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "QUJD");
            }
            _ => panic!("expected base64 image"),
        }
    }

    fn annotated_text(content: &str) -> Node {
        serde_json::from_value(json!({
            "type":"text", "role":"assistant", "id":"text_1", "phase":"answer",
            "content":content, "signature":"signed-original-text",
            "logprobs":[{"token":content,"logprob":-0.2,"top_logprobs":[]}],
            "citations":[citation(Some((0, 1)))], "vendor":"kept"
        }))
        .unwrap()
    }

    fn citation(range: Option<(u64, u64)>) -> Value {
        let mut citation = json!({
            "source":{"kind":"url", "url":"https://example.com/source", "title":"Source"},
            "origin_protocol":"responses", "extra_body":{"vendor":"kept"}
        });
        if let Some((start, end)) = range {
            citation["answer_range"] = json!({"start":start,"end":end});
        }
        citation
    }

    fn run_stream(events: Vec<UrpStreamEvent>) -> Vec<UrpStreamEvent> {
        let mut state = StreamState::default();
        let mut emitted = Vec::new();
        for mut event in events {
            apply_stream(&mut event, &mut state);
            emitted.extend(state.finalize_stream_event(event));
        }
        emitted
    }

    #[test]
    fn unchanged_text_preserves_all_metadata_and_empty_nodes() {
        for text in ["Plain text", "![unfinished", ""] {
            let original = annotated_text(text);
            let mut nodes = vec![original.clone()];
            rewrite_assistant_markdown_images_nodes(&mut nodes);
            assert_eq!(nodes, vec![original]);
        }
    }

    #[test]
    fn unchanged_stream_preserves_header_delta_and_terminal_metadata() {
        for text in ["Plain text", "![unfinished"] {
            let node = annotated_text(text);
            let split = 2;
            let events: Vec<UrpStreamEvent> = vec![
                serde_json::from_value(json!({"event":"node_start","node_index":3,
                    "header":{"type":"text","id":"text_1","role":"assistant","phase":"answer",
                        "signature":"header-signature", "citations":[citation(Some((0,1)))]}, "trace":"start"})).unwrap(),
                serde_json::from_value(json!({"event":"node_delta","node_index":3,
                    "delta":{"type":"text","content":&text[..split],"signature":"delta-signature",
                        "citations":[citation(Some((0,1)))],
                        "logprobs":[{"token":&text[..split],"logprob":-0.2,"top_logprobs":[]}]}, "trace":"delta"})).unwrap(),
                serde_json::from_value(json!({"event":"node_delta","node_index":3,
                    "delta":{"type":"text","content":&text[split..]}})).unwrap(),
                serde_json::from_value(json!({"event":"node_done","node_index":3,"node":node,"trace":"done"})).unwrap(),
                serde_json::from_value(json!({"event":"response_done","output":[node]})).unwrap(),
            ];
            let expected = serde_json::to_value(&events).unwrap();
            assert_eq!(serde_json::to_value(run_stream(events)).unwrap(), expected);
        }
    }

    #[test]
    fn extracted_image_invalidates_signatures_and_remaps_retained_citations() {
        let mut node = annotated_text("A ![x](u) B");
        let Node::Text { citations, .. } = &mut node else {
            panic!()
        };
        *citations = serde_json::from_value(json!([
            citation(Some((0, 1))),
            citation(Some((10, 11))),
            citation(Some((4, 5))),
            citation(None)
        ]))
        .unwrap();
        let mut nodes = vec![node];
        rewrite_assistant_markdown_images_nodes(&mut nodes);
        let Node::Text {
            content,
            signature,
            logprobs,
            citations,
            ..
        } = &nodes[0]
        else {
            panic!()
        };
        assert_eq!(content, "A  B");
        assert!(signature.is_none());
        assert!(logprobs.is_none());
        assert_eq!(
            serde_json::to_value(citations).unwrap(),
            json!([
                citation(Some((0, 1))),
                citation(Some((3, 4))),
                citation(None)
            ])
        );
        assert!(matches!(&nodes[1], Node::Image { .. }));
    }

    #[test]
    fn changed_stream_does_not_replay_metadata_for_deleted_text() {
        let text = "A ![x](u) B";
        let mut node = annotated_text(text);
        let Node::Text { citations, .. } = &mut node else {
            panic!()
        };
        *citations = serde_json::from_value(json!([citation(Some((10, 11)))])).unwrap();
        let events=vec![
            serde_json::from_value(json!({"event":"node_start","node_index":2,
                "header":{"type":"text","id":"text_1","role":"assistant","signature":"original", "citations":[citation(Some((10,11)))]}})).unwrap(),
            serde_json::from_value(json!({"event":"node_delta","node_index":2,
                "delta":{"type":"text","content":text,"signature":"original", "citations":[citation(Some((10,11)))]}})).unwrap(),
            serde_json::from_value(json!({"event":"node_done","node_index":2,"node":node})).unwrap(),
            serde_json::from_value(json!({"event":"response_done","output":[node]})).unwrap(),
        ];
        let emitted = run_stream(events);
        for event in &emitted {
            match event {
                UrpStreamEvent::NodeStart {
                    header:
                        NodeHeader::Text {
                            signature,
                            citations,
                            ..
                        },
                    ..
                } => {
                    assert!(signature.is_none());
                    assert!(citations.is_empty());
                }
                UrpStreamEvent::NodeDelta {
                    delta:
                        NodeDelta::Text {
                            signature,
                            logprobs,
                            citations,
                            ..
                        },
                    ..
                } => {
                    assert!(signature.is_none());
                    assert!(logprobs.is_none());
                    assert!(citations.is_empty());
                }
                _ => {}
            }
        }
        let terminal = emitted
            .iter()
            .find_map(|event| match event {
                UrpStreamEvent::NodeDone {
                    node:
                        Node::Text {
                            content,
                            signature,
                            logprobs,
                            citations,
                            ..
                        },
                    ..
                } => Some((content, signature, logprobs, citations)),
                _ => None,
            })
            .unwrap();
        assert_eq!(terminal.0, "A  B");
        assert!(terminal.1.is_none());
        assert!(terminal.2.is_none());
        assert_eq!(
            serde_json::to_value(terminal.3).unwrap(),
            json!([citation(Some((3, 4)))])
        );
    }
}
