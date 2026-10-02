use crate::transforms::{
    NoState, Phase, Transform, TransformConfig, TransformEntry, TransformError,
    TransformRuntimeContext, TransformScope, TransformState, UrpData,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::any::Any;

#[derive(Debug, Deserialize)]
struct Config {}

impl TransformConfig for Config {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct ReasoningStripConfigTransform;

#[async_trait]
impl Transform for ReasoningStripConfigTransform {
    fn type_id(&self) -> &'static str {
        "reasoning_strip_config"
    }

    fn display_name(&self) -> crate::transforms::LocalizedText {
        &[
            ("en", "Reasoning: strip request config"),
            ("zh", "推理：清除请求配置"),
        ]
    }

    fn display_description(&self) -> crate::transforms::LocalizedText {
        &[
            (
                "en",
                "Drops the request-level reasoning configuration (thinking/effort/budget) before upstream encoding, so the upstream receives a plain non-thinking request. Input reasoning nodes are untouched.",
            ),
            (
                "zh",
                "在上游编码前清除请求级的推理配置（thinking/effort/budget），使上游收到不带思考配置的普通请求。输入中的推理节点不受影响。"),
        ]
    }

    fn supported_phases(&self) -> &'static [Phase] {
        &[Phase::Request]
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
            req.reasoning = None;
        }
        Ok(())
    }
}

inventory::submit!(TransformEntry {
    factory: || Box::new(ReasoningStripConfigTransform),
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::urp::UrpRequest;
    use std::sync::Arc;

    async fn context() -> TransformRuntimeContext {
        // reqwest 0.13 requires an explicit TLS provider in tests.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let temp = tempfile::tempdir().unwrap();
        TransformRuntimeContext {
            image_transform_cache: Arc::new(
                crate::image_transform_cache::ImageTransformCache::new(
                    temp.path().to_path_buf(),
                    std::time::Duration::from_secs(60),
                )
                .await
                .unwrap(),
            ),
            http_client: reqwest::Client::new(),
            upstream_provider_type: None,
            custom_tool_conversions: Default::default(),
        }
    }

    fn request() -> UrpRequest {
        serde_json::from_value(json!({
            "model": "claude-opus-5-5",
            "input": [{"type": "text", "role": "user", "content": "hi"}],
            "reasoning": {"effort": "low"}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn strips_the_request_reasoning_config() {
        let transform = ReasoningStripConfigTransform;
        let config = transform.parse_config(json!({})).unwrap();
        let mut req = request();
        assert!(req.reasoning.is_some());
        transform
            .apply(
                UrpData::Request(&mut req),
                Phase::Request,
                &context().await,
                config.as_ref(),
                transform.init_state().as_mut(),
            )
            .await
            .unwrap();
        assert!(req.reasoning.is_none());
    }

    #[tokio::test]
    async fn noop_without_reasoning_config() {
        let transform = ReasoningStripConfigTransform;
        let config = transform.parse_config(json!({})).unwrap();
        let mut req = request();
        req.reasoning = None;
        transform
            .apply(
                UrpData::Request(&mut req),
                Phase::Request,
                &context().await,
                config.as_ref(),
                transform.init_state().as_mut(),
            )
            .await
            .unwrap();
        assert!(req.reasoning.is_none());
    }
}
