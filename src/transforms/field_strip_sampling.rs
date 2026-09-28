use crate::transforms::{
    NoState, Phase, Transform, TransformConfig, TransformEntry, TransformError,
    TransformRuntimeContext, TransformScope, TransformState, UrpData,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::any::Any;

/// The typed `UrpRequest` fields this transform may clear.
///
/// `field_set` and `field_remove` cannot do this job: SF-5 pins them to `request.extra_body`,
/// while these are typed fields that the encoders emit unconditionally. An upstream that
/// rejects one of them therefore cannot be worked around with the existing `field` transforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SamplingField {
    Temperature,
    TopP,
    Stop,
    Verbosity,
    ParallelToolCalls,
    MaxOutputTokens,
    ResponseFormat,
    User,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    fields: Vec<SamplingField>,
}

impl TransformConfig for Config {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct FieldStripSamplingTransform;

#[async_trait]
impl Transform for FieldStripSamplingTransform {
    fn type_id(&self) -> &'static str {
        "field_strip_sampling"
    }

    fn display_name(&self) -> crate::transforms::LocalizedText {
        &[
            ("en", "Field: strip sampling parameters"),
            ("zh", "字段：移除采样参数"),
        ]
    }

    fn display_description(&self) -> crate::transforms::LocalizedText {
        &[
            (
                "en",
                "Clears the listed typed request fields, such as temperature, before the upstream call. Use it for an upstream or model that rejects a parameter the caller sent.",
            ),
            (
                "zh",
                "在调用上游之前清除列出的类型化请求字段（如 temperature）。用于上游或模型拒绝调用方所发送的某个参数的情况。",
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
                "fields": {
                    "type": "array",
                    "minItems": 1,
                    "uniqueItems": true,
                    "items": {
                        "type": "string",
                        "enum": [
                            "temperature",
                            "top_p",
                            "stop",
                            "verbosity",
                            "parallel_tool_calls",
                            "max_output_tokens",
                            "response_format",
                            "user"
                        ]
                    }
                }
            },
            "required": ["fields"],
            "additionalProperties": false
        })
    }

    fn parse_config(&self, raw: Value) -> Result<Box<dyn TransformConfig>, TransformError> {
        let cfg: Config = serde_json::from_value(raw)
            .map_err(|e| TransformError::InvalidConfig(e.to_string()))?;
        if cfg.fields.is_empty() {
            return Err(TransformError::InvalidConfig(
                "fields must list at least one sampling field".to_string(),
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
        _context: &TransformRuntimeContext,
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

        for field in &cfg.fields {
            match field {
                SamplingField::Temperature => req.temperature = None,
                SamplingField::TopP => req.top_p = None,
                SamplingField::Stop => req.stop = None,
                SamplingField::Verbosity => req.verbosity = None,
                SamplingField::ParallelToolCalls => req.parallel_tool_calls = None,
                SamplingField::MaxOutputTokens => req.max_output_tokens = None,
                SamplingField::ResponseFormat => req.response_format = None,
                SamplingField::User => req.user = None,
            }
        }

        Ok(())
    }
}

inventory::submit!(TransformEntry {
    factory: || Box::new(FieldStripSamplingTransform),
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_transform_cache::ImageTransformCache;
    use crate::urp::{Node, OrdinaryRole, UrpRequest};
    use std::collections::HashMap;
    use tempfile::TempDir;

    async fn context() -> TransformRuntimeContext {
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
            upstream_provider_type: None,
        }
    }

    fn request() -> UrpRequest {
        UrpRequest {
            context: Default::default(),
            image_generation: Default::default(),
            instructions_format: Default::default(),
            logprobs: Default::default(),
            sampling: Default::default(),

            model: "kimi-test".to_string(),
            input: vec![Node::text(OrdinaryRole::User, "hi")],
            stream: Some(false),
            temperature: Some(0.0),
            top_p: Some(0.9),
            max_output_tokens: Some(32),
            reasoning: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: Some(true),
            stop: None,
            verbosity: None,
            response_format: None,
            user: Some("caller".to_string()),
            extra_body: HashMap::new(),
        }
    }

    async fn run(req: &mut UrpRequest, raw_config: Value) {
        let transform = FieldStripSamplingTransform;
        let cfg = transform.parse_config(raw_config).expect("config");
        let mut state = transform.init_state();
        transform
            .apply(
                UrpData::Request(req),
                Phase::Request,
                &context().await,
                cfg.as_ref(),
                state.as_mut(),
            )
            .await
            .expect("apply");
    }

    #[tokio::test]
    async fn clears_only_the_listed_fields() {
        let mut req = request();
        run(&mut req, json!({"fields": ["temperature"]})).await;
        assert_eq!(req.temperature, None);
        assert_eq!(req.top_p, Some(0.9));
        assert_eq!(req.max_output_tokens, Some(32));
        assert_eq!(req.parallel_tool_calls, Some(true));
        assert_eq!(req.user.as_deref(), Some("caller"));
        assert_eq!(req.stream, Some(false));
        assert_eq!(req.input.len(), 1);
    }

    #[tokio::test]
    async fn clears_every_listed_field_and_is_idempotent() {
        let mut req = request();
        let config = json!({
            "fields": ["temperature", "top_p", "max_output_tokens", "parallel_tool_calls", "user"]
        });
        run(&mut req, config.clone()).await;
        assert_eq!(req.temperature, None);
        assert_eq!(req.top_p, None);
        assert_eq!(req.max_output_tokens, None);
        assert_eq!(req.parallel_tool_calls, None);
        assert_eq!(req.user, None);
        run(&mut req, config).await;
        assert_eq!(
            (
                req.temperature,
                req.top_p,
                req.max_output_tokens,
                req.parallel_tool_calls,
                req.user.clone()
            ),
            (None, None, None, None, None)
        );
        // Fields outside the list must survive both applications.
        assert_eq!(req.stream, Some(false));
        assert_eq!(req.input.len(), 1);
    }

    #[tokio::test]
    async fn an_absent_field_is_not_an_error() {
        let mut req = request();
        req.temperature = None;
        run(
            &mut req,
            json!({"fields": ["temperature", "stop", "verbosity"]}),
        )
        .await;
        assert_eq!(req.temperature, None);
        assert_eq!(req.stop, None);
        assert_eq!(req.verbosity, None);
    }

    #[test]
    fn rejects_an_empty_or_unknown_field_list() {
        let transform = FieldStripSamplingTransform;
        assert!(transform.parse_config(json!({"fields": []})).is_err());
        assert!(
            transform
                .parse_config(json!({"fields": ["model"]}))
                .is_err()
        );
        assert!(
            transform
                .parse_config(json!({"fields": ["input"]}))
                .is_err()
        );
        assert!(transform.parse_config(json!({})).is_err());
        assert!(
            transform
                .parse_config(json!({"fields": ["temperature"], "extra": 1}))
                .is_err()
        );
    }
}
