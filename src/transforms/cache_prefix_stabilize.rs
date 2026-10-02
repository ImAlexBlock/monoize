use crate::transforms::{
    NoState, Phase, Transform, TransformConfig, TransformEntry, TransformError,
    TransformRuntimeContext, TransformScope, TransformState, UrpData,
};
use crate::urp::{Node, OrdinaryRole};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::any::Any;

/// Line-delimited blocks that coding agents emit into the system prompt to describe the
/// current machine, working directory, clock, or session. Every one of them changes between
/// requests, and an upstream that caches by token prefix loses everything after the first
/// changed token. Relocating them to a trailing user node, after the conversation, keeps
/// the conversation itself a cacheable prefix. A trailing system node is not enough:
/// Gemini concatenates every system node into `systemInstruction`, and Responses lifts the
/// first system node into `instructions`.
const DEFAULT_BLOCKS: &[(&str, &str)] = &[
    ("<env>", "</env>"),
    ("<environment_context>", "</environment_context>"),
    ("<user_info>", "</user_info>"),
    ("<timestamp>", "</timestamp>"),
];

/// Claude Code writes this per-request billing marker as one line of the system prompt.
const DEFAULT_LINE_PREFIXES: &[&str] = &["x-anthropic-billing-header:"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Relocate,
    Strip,
}

impl Default for Action {
    fn default() -> Self {
        Self::Relocate
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockDelimiter {
    open: String,
    close: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    action: Action,
    /// Absent means the built-in agent block set. An empty array disables block matching.
    #[serde(default)]
    blocks: Option<Vec<BlockDelimiter>>,
    /// Absent means the built-in agent line set. An empty array disables line matching.
    #[serde(default)]
    line_prefixes: Option<Vec<String>>,
    /// Also extract volatile blocks from the first User node that directly follows the
    /// leading System/Developer run. Agents that front-load their metadata into the first
    /// user message (instead of the system prompt) diverge the prefix cache at that node,
    /// so the whole conversation history misses the cache. Default: enabled.
    #[serde(default = "default_true")]
    stabilize_user_preamble: bool,
}

fn default_true() -> bool {
    true
}

impl TransformConfig for Config {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct CachePrefixStabilizeTransform;

#[async_trait]
impl Transform for CachePrefixStabilizeTransform {
    fn type_id(&self) -> &'static str {
        "cache_prefix_stabilize"
    }

    fn display_name(&self) -> crate::transforms::LocalizedText {
        &[
            ("en", "Auto-cache: stabilize the prompt prefix"),
            ("zh", "自动缓存：稳定提示词前缀"),
        ]
    }

    fn display_description(&self) -> crate::transforms::LocalizedText {
        &[
            (
                "en",
                "Moves per-request agent metadata blocks, such as Claude Code <env> or Codex <environment_context>, to a trailing user node after the conversation so an implicit prefix cache can match the history. Relocation keeps every token.",
            ),
            (
                "zh",
                "把每次请求都会变的 agent 元数据块（如 Claude Code 的 <env>、Codex 的 <environment_context>）移到整次请求末尾的 user 节点，使隐式前缀缓存能命中对话历史。搬移不删除任何 token。",
            ),
        ]
    }

    fn supported_phases(&self) -> &'static [Phase] {
        &[Phase::Request]
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
                "action": {"type": "string", "enum": ["relocate", "strip"], "default": "relocate"},
                "blocks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "open": {"type": "string"},
                            "close": {"type": "string"}
                        },
                        "required": ["open", "close"],
                        "additionalProperties": false
                    }
                },
                "line_prefixes": {"type": "array", "items": {"type": "string"}},
                "stabilize_user_preamble": {"type": "boolean", "default": true}
            },
            "additionalProperties": false
        })
    }

    fn parse_config(&self, raw: Value) -> Result<Box<dyn TransformConfig>, TransformError> {
        let cfg: Config = serde_json::from_value(raw)
            .map_err(|e| TransformError::InvalidConfig(e.to_string()))?;
        for block in cfg.blocks.iter().flatten() {
            if block.open.trim().is_empty() || block.close.trim().is_empty() {
                return Err(TransformError::InvalidConfig(
                    "block open and close delimiters must be non-empty".to_string(),
                ));
            }
        }
        if cfg
            .line_prefixes
            .iter()
            .flatten()
            .any(|prefix| prefix.trim().is_empty())
        {
            return Err(TransformError::InvalidConfig(
                "a line prefix must be non-empty".to_string(),
            ));
        }
        Ok(Box::new(cfg))
    }

    fn init_state(&self) -> Box<dyn TransformState> {
        Box::new(NoState)
    }

    async fn apply(
        &self,
        data: UrpData<'_>,
        _phase: Phase,
        context: &TransformRuntimeContext,
        config: &dyn TransformConfig,
        _state: &mut dyn TransformState,
    ) -> Result<(), TransformError> {
        let UrpData::Request(req) = data else {
            return Ok(());
        };
        let cfg = config
            .as_any()
            .downcast_ref::<Config>()
            .ok_or_else(|| TransformError::InvalidConfig("unexpected config type".to_string()))?;

        // An Anthropic upstream caches at an explicit `cache_control` breakpoint, which is a
        // whole-node marker. Reordering text inside a node cannot move that boundary, so this
        // transform would change the prompt without changing what gets cached.
        if !matches!(
            context.upstream_provider_type,
            Some(crate::config::ProviderType::ChatCompletion)
                | Some(crate::config::ProviderType::Responses)
        ) {
            return Ok(());
        }

        let blocks: Vec<(&str, &str)> = match cfg.blocks.as_ref() {
            Some(configured) => configured
                .iter()
                .map(|block| (block.open.as_str(), block.close.as_str()))
                .collect(),
            None => DEFAULT_BLOCKS.to_vec(),
        };
        let line_prefixes: Vec<&str> = match cfg.line_prefixes.as_ref() {
            Some(configured) => configured.iter().map(String::as_str).collect(),
            None => DEFAULT_LINE_PREFIXES.to_vec(),
        };
        if blocks.is_empty() && line_prefixes.is_empty() {
            return Ok(());
        }

        // The cacheable prefix is the leading run of System/Developer nodes, the same
        // definition `cache_openai_prompt` uses to build its key material. A volatile segment
        // left at the end of that run still sits in front of the conversation. A trailing
        // System node is also not enough: Gemini concatenates every System/Developer node
        // into `systemInstruction`, and Responses lifts the first such node into
        // `instructions`. Relocate therefore appends a User node after every existing
        // input node, which every OpenAI-family encoder leaves at the end of the request.
        let prefix_len = req
            .input
            .iter()
            .position(|node| {
                !matches!(
                    node.role(),
                    Some(OrdinaryRole::System | OrdinaryRole::Developer)
                )
            })
            .unwrap_or(req.input.len());
        if prefix_len == 0 {
            return Ok(());
        }

        // The user preamble region: the first User node directly after the leading
        // System/Developer run. Agents such as hermes front-load per-request metadata
        // (clock, environment, run id) into that node instead of the system prompt, which
        // diverges an implicit prefix cache at the first conversation node. Blocks and
        // lines matched here are extracted with the same rules as the system region.
        let mut scan_end = prefix_len;
        if cfg.stabilize_user_preamble
            && matches!(
                req.input.get(prefix_len).map(Node::role),
                Some(Some(OrdinaryRole::User))
            )
        {
            scan_end = prefix_len + 1;
        }

        let mut volatile: Vec<String> = Vec::new();
        for node in req.input[..scan_end].iter_mut() {
            let Node::Text { content, .. } = node else {
                continue;
            };
            let extracted = extract_volatile_lines(content, &blocks, &line_prefixes);
            if extracted.is_empty() {
                continue;
            }
            volatile.extend(extracted);
        }
        if volatile.is_empty() {
            return Ok(());
        }

        let mut index = 0usize;
        req.input.retain(|node| {
            let keep = !matches!(node, Node::Text { content, .. } if index < scan_end && content.is_empty());
            index += 1;
            keep
        });

        if cfg.action == Action::Relocate {
            req.input
                .push(Node::text(OrdinaryRole::User, volatile.join("\n")));
        }

        Ok(())
    }
}

/// Removes every volatile line from `content` in place and returns those lines in their
/// original order.
///
/// Matching is line-oriented because every agent emits these markers on their own lines, and
/// because a line boundary is the only split that keeps the operation idempotent: the removed
/// lines are re-joined with a single newline, so extracting an already-relocated tail and
/// appending it again reproduces the same string.
///
/// An opening block delimiter with no closing delimiter later in the same node does NOT match.
/// Swallowing the remainder of a system prompt on a malformed marker would be far worse than
/// leaving the prompt alone.
fn extract_volatile_lines(
    content: &mut String,
    blocks: &[(&str, &str)],
    line_prefixes: &[&str],
) -> Vec<String> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut volatile_span = vec![false; lines.len()];
    let mut index = 0usize;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if line_prefixes
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
        {
            volatile_span[index] = true;
            index += 1;
            continue;
        }
        let opened = blocks
            .iter()
            .find(|(open, _)| trimmed.starts_with(open))
            .map(|(_, close)| *close);
        if let Some(close) = opened {
            let end =
                (index..lines.len()).find(|candidate| lines[*candidate].trim().ends_with(close));
            if let Some(end) = end {
                for position in index..=end {
                    volatile_span[position] = true;
                }
                index = end + 1;
                continue;
            }
        }
        index += 1;
    }

    if !volatile_span.iter().any(|flagged| *flagged) {
        return Vec::new();
    }

    let removed: Vec<String> = lines
        .iter()
        .zip(&volatile_span)
        .filter(|(_, flagged)| **flagged)
        .map(|(line, _)| (*line).to_string())
        .collect();
    let kept: Vec<&str> = lines
        .iter()
        .zip(&volatile_span)
        .filter(|(_, flagged)| !**flagged)
        .map(|(line, _)| *line)
        .collect();
    *content = kept.join("\n").trim_matches('\n').to_string();
    removed
}

inventory::submit!(TransformEntry {
    factory: || Box::new(CachePrefixStabilizeTransform),
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderType;
    use crate::image_transform_cache::ImageTransformCache;
    use crate::urp::UrpRequest;
    use std::collections::HashMap;
    use tempfile::TempDir;

    async fn context(upstream: Option<ProviderType>) -> TransformRuntimeContext {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let temp_dir = TempDir::new().expect("temp dir");
        let cache = ImageTransformCache::new(
            temp_dir.path().join("cache"),
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("cache");
        TransformRuntimeContext {
            image_transform_cache: std::sync::Arc::new(cache),
            http_client: reqwest::Client::new(),
            upstream_provider_type: upstream,
            custom_tool_conversions: Default::default(),
        }
    }

    fn request(input: Vec<Node>) -> UrpRequest {
        UrpRequest {
            context: Default::default(),
            image_generation: Default::default(),
            instructions_format: Default::default(),
            logprobs: Default::default(),
            sampling: Default::default(),

            model: "glm-test".to_string(),
            input,
            stream: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            stop: None,
            verbosity: None,
            response_format: None,
            user: None,
            extra_body: HashMap::new(),
        }
    }

    async fn run(req: &mut UrpRequest, raw_config: Value, upstream: Option<ProviderType>) {
        let transform = CachePrefixStabilizeTransform;
        let cfg = transform.parse_config(raw_config).expect("config");
        let mut state = transform.init_state();
        transform
            .apply(
                UrpData::Request(req),
                Phase::Request,
                &context(upstream).await,
                cfg.as_ref(),
                state.as_mut(),
            )
            .await
            .expect("apply");
    }

    fn text_of(node: &Node) -> &str {
        match node {
            Node::Text { content, .. } => content,
            other => panic!("expected a text node, got {other:?}"),
        }
    }

    const AGENT_SYSTEM: &str = "You are a coding agent.\n\
<env>\nWorking directory: /workspace\nToday's date: 2026-09-10\n</env>\n\
## Rules\nPrefer the existing convention.";

    #[tokio::test]
    async fn relocates_the_agent_environment_block_behind_the_conversation() {
        let mut req = request(vec![
            Node::text(OrdinaryRole::System, AGENT_SYSTEM),
            Node::text(OrdinaryRole::User, "hello"),
            Node::assistant_text("hi"),
        ]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;

        assert_eq!(req.input.len(), 4);
        assert_eq!(
            text_of(&req.input[0]),
            "You are a coding agent.\n## Rules\nPrefer the existing convention."
        );
        assert_eq!(text_of(&req.input[1]), "hello");
        assert_eq!(text_of(&req.input[2]), "hi");
        assert_eq!(
            text_of(&req.input[3]),
            "<env>\nWorking directory: /workspace\nToday's date: 2026-09-10\n</env>"
        );
        assert_eq!(
            req.input[3].role(),
            Some(OrdinaryRole::User),
            "the relocated block must be a trailing user node so Gemini and Responses cannot hoist it"
        );
    }

    #[tokio::test]
    async fn extracts_volatile_blocks_from_the_first_user_preamble() {
        // hermes layout: static system, then the first user message front-loads an
        // environment block that changes per request, then the conversation.
        let mut req = request(vec![
            Node::text(OrdinaryRole::System, "You are a coding agent."),
            Node::text(
                OrdinaryRole::User,
                "<env>\nToday's date: 2026-09-19\nRun id: 7f3a\n</env>\nFind the bug in main.rs.",
            ),
            Node::assistant_text("I found it."),
            Node::text(OrdinaryRole::User, "Fix it."),
        ]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;

        assert_eq!(req.input.len(), 5);
        assert_eq!(text_of(&req.input[0]), "You are a coding agent.");
        assert_eq!(
            text_of(&req.input[1]),
            "Find the bug in main.rs.",
            "the stable part of the first user message stays in place"
        );
        assert_eq!(text_of(&req.input[2]), "I found it.");
        assert_eq!(text_of(&req.input[3]), "Fix it.");
        assert_eq!(
            text_of(&req.input[4]),
            "<env>\nToday's date: 2026-09-19\nRun id: 7f3a\n</env>"
        );
        assert_eq!(req.input[4].role(), Some(OrdinaryRole::User));
    }

    #[tokio::test]
    async fn user_preamble_extraction_is_idempotent() {
        let mut req = request(vec![
            Node::text(OrdinaryRole::System, "You are a coding agent."),
            Node::text(
                OrdinaryRole::User,
                "<env>\nRun id: 7f3a\n</env>\nFind the bug.",
            ),
            Node::text(OrdinaryRole::User, "Fix it."),
        ]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;
        let after_first: Vec<String> = req
            .input
            .iter()
            .map(|node| text_of(node).to_string())
            .collect();
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;
        let after_second: Vec<String> = req
            .input
            .iter()
            .map(|node| text_of(node).to_string())
            .collect();
        assert_eq!(after_first, after_second);
        assert_eq!(after_second[1], "Find the bug.");
    }

    #[tokio::test]
    async fn stabilize_user_preamble_false_keeps_the_first_user_message_untouched() {
        let mut req = request(vec![
            Node::text(OrdinaryRole::System, "You are a coding agent."),
            Node::text(
                OrdinaryRole::User,
                "<env>\nRun id: 7f3a\n</env>\nFind the bug.",
            ),
        ]);
        run(
            &mut req,
            json!({"stabilize_user_preamble": false}),
            Some(ProviderType::ChatCompletion),
        )
        .await;

        assert_eq!(
            text_of(&req.input[1]),
            "<env>\nRun id: 7f3a\n</env>\nFind the bug.",
            "ACPS-24: with the flag off, user nodes are never read or written"
        );
        assert_eq!(req.input.len(), 2);
    }

    #[tokio::test]
    async fn a_volatile_block_in_a_later_user_node_is_never_extracted() {
        let mut req = request(vec![
            Node::text(OrdinaryRole::System, "You are a coding agent."),
            Node::text(OrdinaryRole::User, "hello"),
            Node::assistant_text("hi"),
            Node::text(
                OrdinaryRole::User,
                "<env>\nRun id: 7f3a\n</env>\nNext question.",
            ),
        ]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;

        assert_eq!(
            text_of(&req.input[3]),
            "<env>\nRun id: 7f3a\n</env>\nNext question.",
            "ACPS-23: only the first user node after the stable prefix is eligible"
        );
        assert_eq!(
            req.input.len(),
            4,
            "nothing volatile found, no trailing node"
        );
    }

    #[tokio::test]
    async fn relocation_is_idempotent() {
        let mut once = request(vec![
            Node::text(OrdinaryRole::System, AGENT_SYSTEM),
            Node::text(OrdinaryRole::User, "hello"),
        ]);
        run(&mut once, json!({}), Some(ProviderType::ChatCompletion)).await;
        let after_first: Vec<(OrdinaryRole, String)> = once
            .input
            .iter()
            .map(|node| (node.role().expect("role"), text_of(node).to_string()))
            .collect();
        assert_eq!(after_first.len(), 3);
        assert_eq!(after_first[2].0, OrdinaryRole::User);
        run(&mut once, json!({}), Some(ProviderType::ChatCompletion)).await;
        let after_second: Vec<(OrdinaryRole, String)> = once
            .input
            .iter()
            .map(|node| (node.role().expect("role"), text_of(node).to_string()))
            .collect();
        assert_eq!(after_second, after_first);
    }

    #[tokio::test]
    async fn strips_when_configured_and_drops_a_node_that_becomes_empty() {
        let mut req = request(vec![
            Node::text(
                OrdinaryRole::System,
                "<user_info>\nWorkspace Path: /w\n</user_info>",
            ),
            Node::text(OrdinaryRole::System, "Stable instructions."),
            Node::text(OrdinaryRole::User, "hi"),
        ]);
        run(
            &mut req,
            json!({"action": "strip"}),
            Some(ProviderType::ChatCompletion),
        )
        .await;

        assert_eq!(req.input.len(), 2);
        assert_eq!(text_of(&req.input[0]), "Stable instructions.");
        assert_eq!(text_of(&req.input[1]), "hi");
    }

    #[tokio::test]
    async fn collects_across_prefix_nodes_and_appends_a_trailing_user_node_in_order() {
        let mut req = request(vec![
            Node::text(
                OrdinaryRole::System,
                "First.\nx-anthropic-billing-header: cch=a1;",
            ),
            Node::text(
                OrdinaryRole::Developer,
                "<timestamp>\nnow\n</timestamp>\nSecond.",
            ),
            Node::text(OrdinaryRole::User, "go"),
        ]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;

        assert_eq!(req.input.len(), 4);
        assert_eq!(text_of(&req.input[0]), "First.");
        assert_eq!(text_of(&req.input[1]), "Second.");
        assert_eq!(text_of(&req.input[2]), "go");
        assert_eq!(
            text_of(&req.input[3]),
            "x-anthropic-billing-header: cch=a1;\n<timestamp>\nnow\n</timestamp>"
        );
        assert_eq!(req.input[3].role(), Some(OrdinaryRole::User));
    }

    #[tokio::test]
    async fn leaves_the_prompt_alone_when_a_block_is_never_closed() {
        let unterminated = "Instructions.\n<env>\nWorking directory: /workspace";
        let mut req = request(vec![Node::text(OrdinaryRole::System, unterminated)]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;
        assert_eq!(text_of(&req.input[0]), unterminated);
    }

    #[tokio::test]
    async fn never_touches_user_or_assistant_content() {
        let user_text = "<env>\nthis is my own text\n</env>";
        let mut req = request(vec![
            Node::text(OrdinaryRole::System, "Instructions."),
            Node::text(OrdinaryRole::User, user_text),
        ]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;
        assert_eq!(text_of(&req.input[0]), "Instructions.");
        assert_eq!(text_of(&req.input[1]), user_text);
    }

    #[tokio::test]
    async fn does_not_reach_a_system_node_that_follows_the_leading_run() {
        let trailing = "<env>\nlate\n</env>";
        let mut req = request(vec![
            Node::text(OrdinaryRole::System, "Instructions."),
            Node::text(OrdinaryRole::User, "hi"),
            Node::text(OrdinaryRole::System, trailing),
        ]);
        run(&mut req, json!({}), Some(ProviderType::ChatCompletion)).await;
        assert_eq!(text_of(&req.input[2]), trailing);
    }

    #[tokio::test]
    async fn is_a_no_op_for_an_anthropic_upstream_and_when_no_upstream_is_selected() {
        for upstream in [Some(ProviderType::Messages), None] {
            let mut req = request(vec![Node::text(OrdinaryRole::System, AGENT_SYSTEM)]);
            run(&mut req, json!({}), upstream).await;
            assert_eq!(text_of(&req.input[0]), AGENT_SYSTEM);
        }
    }

    #[tokio::test]
    async fn an_empty_pattern_set_disables_the_transform() {
        let mut req = request(vec![Node::text(OrdinaryRole::System, AGENT_SYSTEM)]);
        run(
            &mut req,
            json!({"blocks": [], "line_prefixes": []}),
            Some(ProviderType::ChatCompletion),
        )
        .await;
        assert_eq!(text_of(&req.input[0]), AGENT_SYSTEM);
    }

    #[tokio::test]
    async fn a_custom_block_matches_an_unlisted_agent() {
        let mut req = request(vec![Node::text(
            OrdinaryRole::System,
            "<opencode-context>\ncwd=/w\n</opencode-context>\nStable rules.\nMore rules.",
        )]);
        run(
            &mut req,
            json!({"blocks": [{"open": "<opencode-context>", "close": "</opencode-context>"}]}),
            Some(ProviderType::ChatCompletion),
        )
        .await;
        assert_eq!(text_of(&req.input[0]), "Stable rules.\nMore rules.");
        assert_eq!(
            text_of(&req.input[1]),
            "<opencode-context>\ncwd=/w\n</opencode-context>"
        );
        assert_eq!(req.input[1].role(), Some(OrdinaryRole::User));
        // The built-in set must not be consulted once the caller supplies one.
        let mut billing = request(vec![Node::text(
            OrdinaryRole::System,
            "x-anthropic-billing-header: cch=a1;\nStable rules.",
        )]);
        run(
            &mut billing,
            json!({"blocks": [{"open": "<opencode-context>", "close": "</opencode-context>"}],
                   "line_prefixes": []}),
            Some(ProviderType::ChatCompletion),
        )
        .await;
        assert_eq!(
            text_of(&billing.input[0]),
            "x-anthropic-billing-header: cch=a1;\nStable rules."
        );
    }

    #[test]
    fn rejects_an_empty_delimiter_or_prefix() {
        let transform = CachePrefixStabilizeTransform;
        assert!(
            transform
                .parse_config(json!({"blocks": [{"open": " ", "close": "</x>"}]}))
                .is_err()
        );
        assert!(
            transform
                .parse_config(json!({"line_prefixes": [""]}))
                .is_err()
        );
        assert!(transform.parse_config(json!({"action": "nope"})).is_err());
    }

    fn encoded_chat_prefix(req: &UrpRequest) -> String {
        crate::urp::encode::openai_chat::encode_request(req, "probe")["messages"].to_string()
    }

    fn common_prefix_len(left: &str, right: &str) -> usize {
        left.bytes()
            .zip(right.bytes())
            .take_while(|(a, b)| a == b)
            .count()
    }

    fn growing_agent_turns(env_clocks: &[&str]) -> Vec<UrpRequest> {
        let mut history = vec![
            Node::text(OrdinaryRole::User, "load workspace"),
            Node::assistant_text("loaded"),
        ];
        env_clocks
            .iter()
            .enumerate()
            .map(|(index, clock)| {
                let system = format!(
                    "You are a coding agent.\n<env>\nWorking directory: /workspace\nClock: {clock}\n</env>\n## Rules\nPrefer the existing convention."
                );
                let mut input = vec![Node::text(OrdinaryRole::System, system)];
                input.extend(history.clone());
                input.push(Node::text(
                    OrdinaryRole::User,
                    format!("continue turn {}", index + 1),
                ));
                history.push(Node::text(
                    OrdinaryRole::User,
                    format!("continue turn {}", index + 1),
                ));
                history.push(Node::assistant_text(format!("ack {}", index + 1)));
                request(input)
            })
            .collect()
    }

    #[tokio::test]
    async fn growing_conversation_extends_the_encoded_prefix_instead_of_pinning_it() {
        let clocks = ["t1", "t2", "t3", "t4"];
        let mut rewritten = growing_agent_turns(&clocks);
        for req in &mut rewritten {
            run(req, json!({}), Some(ProviderType::ChatCompletion)).await;
            assert_eq!(
                req.input.last().and_then(Node::role),
                Some(OrdinaryRole::User)
            );
        }
        let rewritten_bodies: Vec<String> = rewritten.iter().map(encoded_chat_prefix).collect();
        let original_bodies: Vec<String> = growing_agent_turns(&clocks)
            .iter()
            .map(encoded_chat_prefix)
            .collect();

        let mut original_shared = common_prefix_len(&original_bodies[0], &original_bodies[1]);
        let mut rewritten_shared = common_prefix_len(&rewritten_bodies[0], &rewritten_bodies[1]);
        for index in 2..rewritten_bodies.len() {
            let next_original =
                common_prefix_len(&original_bodies[index - 1], &original_bodies[index]);
            let next_rewritten =
                common_prefix_len(&rewritten_bodies[index - 1], &rewritten_bodies[index]);
            assert_eq!(
                next_original, original_shared,
                "the old system-tail layout must pin the shared prefix"
            );
            assert!(
                next_rewritten > rewritten_shared,
                "the trailing-user layout must grow the shared prefix as history grows: {rewritten_shared} then {next_rewritten}"
            );
            original_shared = next_original;
            rewritten_shared = next_rewritten;
        }
        assert!(
            rewritten_shared > original_shared,
            "relocated history must share more prefix bytes than the pinned system tail"
        );
    }
}
