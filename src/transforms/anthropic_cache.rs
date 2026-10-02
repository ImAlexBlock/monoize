//! Shared `ttl` config and marker ownership for the Anthropic `cache_*` transforms
//! (`spec/auto-cache-transforms.spec.md` DEF-11 through DEF-15).

use crate::transforms::{TransformConfig, TransformError};
use crate::urp::{AnthropicCacheMarker, AnthropicCacheTarget, UrpRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::any::Any;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
enum CacheTtl {
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct CacheConfig {
    ttl: CacheTtl,
}

impl TransformConfig for CacheConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl CacheConfig {
    pub(super) fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "ttl": {
                    "type": "string",
                    "enum": ["5m", "1h"],
                    "default": "5m"
                }
            },
            "additionalProperties": false
        })
    }

    pub(super) fn parse(raw: Value) -> Result<Box<dyn TransformConfig>, TransformError> {
        let cfg: Self = serde_json::from_value(raw)
            .map_err(|e| TransformError::InvalidConfig(e.to_string()))?;
        Ok(Box::new(cfg))
    }

    pub(super) fn from_dyn(config: &dyn TransformConfig) -> Result<&Self, TransformError> {
        config
            .as_any()
            .downcast_ref::<Self>()
            .ok_or_else(|| TransformError::Apply("invalid config type".to_string()))
    }

    /// Applies this rule's marker to one target. A marker inserted by an earlier Monoize rule is
    /// retuned, so later scopes (API key after Provider) win; a client marker is left untouched.
    /// `slot_free` gates only the creation of a new breakpoint.
    pub(super) fn apply_marker(
        &self,
        req: &mut UrpRequest,
        target: AnthropicCacheTarget,
        node_index: Option<usize>,
        slot_free: bool,
    ) {
        let owned = req
            .context
            .anthropic_cache_markers
            .get(&target)
            .is_some_and(|snapshot| marker_snapshot(req, node_index).as_ref() == Some(snapshot));
        if !owned {
            req.context.anthropic_cache_markers.remove(&target);
        }
        let extra_body = match node_index {
            Some(index) => req.input[index].extra_body_mut(),
            None => &mut req.extra_body,
        };
        match extra_body.get_mut("cache_control") {
            Some(marker) if owned => *marker = self.cache_control(),
            Some(_) => return,
            None if slot_free => {
                extra_body.insert("cache_control".to_string(), self.cache_control());
            }
            None => return,
        }
        if let Some(snapshot) = marker_snapshot(req, node_index) {
            req.context.anthropic_cache_markers.insert(target, snapshot);
        } else {
            req.context.anthropic_cache_markers.remove(&target);
        }
    }

    /// `5m` omits `ttl` because Anthropic defaults to a 5-minute TTL, which keeps the wire
    /// format of older rules.
    fn cache_control(&self) -> Value {
        match self.ttl {
            CacheTtl::FiveMinutes => json!({"type": "ephemeral"}),
            CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
        }
    }
}

fn marker_snapshot(req: &UrpRequest, node_index: Option<usize>) -> Option<AnthropicCacheMarker> {
    match node_index {
        Some(index) => {
            let node = req.input.get(index)?;
            // Equal nodes can exchange positions without changing either fingerprint.
            if req
                .input
                .iter()
                .enumerate()
                .any(|(other, value)| other != index && value == node)
            {
                return None;
            }
            Some(AnthropicCacheMarker {
                node_index,
                node_count: req.input.len(),
                fingerprint: fingerprint(node),
            })
        }
        None => Some(AnthropicCacheMarker {
            node_index: None,
            node_count: 0,
            fingerprint: fingerprint(req.extra_body.get("cache_control")?),
        }),
    }
}

fn fingerprint(value: &impl Serialize) -> [u8; 32] {
    struct DigestWriter(Sha256);
    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).expect("URP cache targets serialize as JSON");
    writer.0.finalize().into()
}
