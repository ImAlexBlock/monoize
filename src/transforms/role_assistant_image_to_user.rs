use crate::transforms::{
    NoState, Phase, Transform, TransformConfig, TransformEntry, TransformError,
    TransformRuntimeContext, TransformScope, TransformState, UrpData,
};
use crate::urp::{Node, OrdinaryRole};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::any::Any;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {}

impl TransformConfig for Config {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct RoleAssistantImageToUserTransform;

#[async_trait]
impl Transform for RoleAssistantImageToUserTransform {
    fn type_id(&self) -> &'static str {
        "role_assistant_image_to_user"
    }

    fn display_name(&self) -> crate::transforms::LocalizedText {
        &[
            ("en", "Role: assistant image to user"),
            ("zh", "角色：assistant 图片移至 user 消息"),
        ]
    }

    fn display_description(&self) -> crate::transforms::LocalizedText {
        &[
            (
                "en",
                "Moves images attached to assistant messages onto the following user message, because some upstreams silently drop assistant-turn images.",
            ),
            (
                "zh",
                "把 assistant 消息携带的图片移动到后一条 user 消息，避免部分上游静默丢弃 assistant 轮图片。",
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
        Box::new(NoState)
    }

    async fn apply(
        &self,
        data: UrpData<'_>,
        _phase: Phase,
        _context: &TransformRuntimeContext,
        _config: &dyn TransformConfig,
        _state: &mut dyn TransformState,
    ) -> Result<(), TransformError> {
        if let UrpData::Request(req) = data {
            req.input = relocate_assistant_images(&req.input);
        }
        Ok(())
    }
}

fn is_user_ordinary_node(node: &Node) -> bool {
    matches!(
        node,
        Node::Text { role, .. }
        | Node::Image { role, .. }
        | Node::Audio { role, .. }
        | Node::File { role, .. }
        | Node::ProviderItem { role, .. }
            if *role == OrdinaryRole::User,
    )
}

fn relocate_assistant_images(nodes: &[Node]) -> Vec<Node> {
    // Anchors and removal positions are computed in original coordinates so
    // relocation can never strand an image between an assistant ToolCall and
    // its ToolResult.
    let next_user_anchor: Vec<Option<usize>> = {
        let mut anchors = vec![None; nodes.len() + 1];
        for idx in (0..nodes.len()).rev() {
            anchors[idx] = if is_user_ordinary_node(&nodes[idx]) {
                Some(idx)
            } else {
                anchors[idx + 1]
            };
        }
        anchors
    };

    let mut insertions: Vec<(usize, Node)> = Vec::new();
    let mut appends: Vec<Node> = Vec::new();
    let mut removed: Vec<bool> = vec![false; nodes.len()];
    let mut found_any = false;

    for (idx, node) in nodes.iter().enumerate() {
        let Node::Image { role, .. } = node else {
            continue;
        };
        if *role != OrdinaryRole::Assistant {
            continue;
        }
        found_any = true;
        removed[idx] = true;
        let mut relocated = node.clone();
        if let Node::Image { id, role, .. } = &mut relocated {
            *id = None;
            *role = OrdinaryRole::User;
        }
        match next_user_anchor.get(idx + 1).copied().flatten() {
            Some(anchor) => insertions.push((anchor, relocated)),
            None => appends.push(relocated),
        }
    }

    if !found_any {
        return nodes.to_vec();
    }

    // insertions are pushed in ascending original index, so images sharing an
    // anchor keep their original relative order at that anchor.
    insertions.sort_by_key(|(anchor, _)| *anchor);
    let mut insertion_iter = insertions.into_iter().peekable();

    let mut out = Vec::with_capacity(nodes.len());
    for (idx, node) in nodes.iter().enumerate() {
        while insertion_iter
            .peek()
            .is_some_and(|(anchor, _)| *anchor == idx)
        {
            let (_, image) = insertion_iter.next().expect("peeked insertion exists");
            out.push(image);
        }
        if removed[idx] {
            continue;
        }
        out.push(node.clone());
    }
    out.extend(appends);
    out
}

inventory::submit!(TransformEntry {
    factory: || Box::new(RoleAssistantImageToUserTransform),
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transforms::text_node;
    use crate::urp::{AudioSource, FileSource, ImageSource};
    use std::collections::HashMap;

    fn image_node(role: OrdinaryRole, marker: &str) -> Node {
        Node::Image {
            metadata: Default::default(),

            id: Some(format!("img-{marker}")),
            role,
            source: ImageSource::Url {
                url: format!("https://example.invalid/{marker}.png"),
                detail: None,
            },
            extra_body: HashMap::new(),
        }
    }

    fn text_node_with_id(role: OrdinaryRole, content: &str) -> Node {
        Node::Text {
            citations: Default::default(),
            logprobs: Default::default(),
            signature: Default::default(),

            id: None,
            role,
            content: content.to_string(),
            phase: None,
            extra_body: HashMap::new(),
        }
    }

    fn node_summary(node: &Node) -> String {
        match node {
            Node::Text { role, content, .. } => format!("text:{role:?}:{content}"),
            Node::Image { role, id, .. } => format!("image:{role:?}:{id:?}"),
            Node::ToolCall { name, .. } => format!("tool_call:{name}"),
            Node::ToolResult { call_id, .. } => format!("tool_result:{call_id}"),
            _ => "other".to_string(),
        }
    }

    fn summarize(nodes: &[Node]) -> Vec<String> {
        nodes.iter().map(node_summary).collect()
    }

    #[test]
    fn moves_assistant_image_before_following_user_message() {
        let nodes = vec![
            text_node(OrdinaryRole::User, "sharing now"),
            text_node_with_id(OrdinaryRole::Assistant, "shown below"),
            image_node(OrdinaryRole::Assistant, "a"),
            text_node(OrdinaryRole::User, "what color is it?"),
        ];

        let out = relocate_assistant_images(&nodes);
        assert_eq!(
            summarize(&out),
            vec![
                "text:User:sharing now",
                "text:Assistant:shown below",
                "image:User:None",
                "text:User:what color is it?",
            ]
        );
        // Payload survives the move.
        match &out[2] {
            Node::Image { source, .. } => assert!(matches!(
                source,
                ImageSource::Url { url, .. } if url == "https://example.invalid/a.png"
            )),
            _ => panic!("expected relocated image node"),
        }
    }

    #[test]
    fn appends_at_end_when_no_following_user_message_exists() {
        let nodes = vec![
            text_node(OrdinaryRole::User, "q"),
            text_node_with_id(OrdinaryRole::Assistant, "a"),
            image_node(OrdinaryRole::Assistant, "tail"),
        ];

        let out = relocate_assistant_images(&nodes);
        assert_eq!(
            summarize(&out),
            vec!["text:User:q", "text:Assistant:a", "image:User:None"]
        );
    }

    #[test]
    fn skips_tool_nodes_and_lands_on_the_next_user_run() {
        let tool_call = Node::ToolCall {
            namespace: Default::default(),
            signature: Default::default(),

            id: None,
            tool_type: crate::urp::ToolCallType::Function,
            call_id: "call-1".to_string(),
            name: "lookup".to_string(),
            arguments: "{}".to_string(),
            extra_body: HashMap::new(),
        };
        let tool_result = Node::ToolResult {
            name: Default::default(),
            namespace: Default::default(),
            signature: Default::default(),

            id: None,
            tool_type: crate::urp::ToolCallType::Function,
            call_id: "call-1".to_string(),
            is_error: false,
            content: Vec::new(),
            extra_body: HashMap::new(),
        };
        let nodes = vec![
            text_node_with_id(OrdinaryRole::Assistant, "let me check"),
            image_node(OrdinaryRole::Assistant, "mid"),
            tool_call,
            tool_result,
            text_node(OrdinaryRole::User, "thanks"),
        ];

        let out = relocate_assistant_images(&nodes);
        assert_eq!(
            summarize(&out),
            vec![
                "text:Assistant:let me check",
                "tool_call:lookup",
                "tool_result:call-1",
                "image:User:None",
                "text:User:thanks",
            ]
        );
    }

    #[test]
    fn keeps_relative_order_of_images_sharing_one_anchor() {
        let nodes = vec![
            text_node_with_id(OrdinaryRole::Assistant, "turn one"),
            image_node(OrdinaryRole::Assistant, "first"),
            text_node_with_id(OrdinaryRole::Assistant, "turn two"),
            image_node(OrdinaryRole::Assistant, "second"),
            text_node(OrdinaryRole::User, "go"),
        ];

        let out = relocate_assistant_images(&nodes);
        let image_urls: Vec<String> = out
            .iter()
            .filter_map(|node| match node {
                Node::Image { source, .. } => match source {
                    ImageSource::Url { url, .. } => Some(url.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(
            image_urls,
            vec![
                "https://example.invalid/first.png",
                "https://example.invalid/second.png",
            ]
        );
        assert_eq!(node_summary(&out[4]), "text:User:go");
    }

    #[test]
    fn is_idempotent() {
        let nodes = vec![
            text_node_with_id(OrdinaryRole::Assistant, "a"),
            image_node(OrdinaryRole::Assistant, "x"),
            text_node(OrdinaryRole::User, "q"),
        ];
        let once = relocate_assistant_images(&nodes);
        let twice = relocate_assistant_images(&once);
        assert_eq!(summarize(&once), summarize(&twice));
    }

    #[test]
    fn leaves_non_assistant_images_and_other_media_untouched() {
        let user_image = image_node(OrdinaryRole::User, "keep");
        let system_image = image_node(OrdinaryRole::System, "sys");
        let audio = Node::Audio {
            metadata: Default::default(),

            id: None,
            role: OrdinaryRole::Assistant,
            source: AudioSource::Url {
                url: "https://example.invalid/a.mp3".to_string(),
            },
            extra_body: HashMap::new(),
        };
        let file = Node::File {
            metadata: Default::default(),

            id: None,
            role: OrdinaryRole::Assistant,
            source: FileSource::FileId {
                file_id: "file-1".to_string(),
            },
            extra_body: HashMap::new(),
        };
        let nodes = vec![
            system_image,
            user_image.clone(),
            audio.clone(),
            file.clone(),
            text_node(OrdinaryRole::User, "q"),
        ];

        let out = relocate_assistant_images(&nodes);
        assert_eq!(out.len(), nodes.len());
        assert_eq!(summarize(&out), summarize(&nodes));
    }

    #[test]
    fn returns_input_unchanged_when_no_assistant_images_exist() {
        let nodes = vec![
            text_node(OrdinaryRole::User, "q"),
            text_node_with_id(OrdinaryRole::Assistant, "a"),
        ];
        let out = relocate_assistant_images(&nodes);
        assert_eq!(summarize(&out), summarize(&nodes));
    }

    #[test]
    fn rejects_configs_with_any_key() {
        let transform = RoleAssistantImageToUserTransform;
        assert!(transform.parse_config(json!({})).is_ok());
        assert!(transform.parse_config(json!({"unexpected": 1})).is_err());
    }
}
