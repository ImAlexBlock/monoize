use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;

pub(crate) const DEFAULT_UPSTREAM_DISCOVERY_MAX_BYTES: usize = 16_777_216;
const UPSTREAM_DISCOVERY_MAX_BYTES_ENV: &str = "MONOIZE_UPSTREAM_DISCOVERY_MAX_BYTES";

#[derive(Debug, thiserror::Error)]
pub(crate) enum BoundedResponseError {
    #[error(
        "upstream discovery response exceeds the {max_bytes}-byte limit: Content-Length is {content_length}"
    )]
    DeclaredLengthExceeded {
        content_length: u64,
        max_bytes: usize,
    },
    #[error(
        "upstream discovery response exceeds the {max_bytes}-byte limit while reading the body"
    )]
    StreamedLengthExceeded { max_bytes: usize },
    #[error("failed to read upstream discovery response body: {source}")]
    BodyRead {
        #[source]
        source: reqwest::Error,
    },
    #[error("upstream response body idle for {idle_ms}ms without data")]
    Idle { idle_ms: u128 },
}

impl BoundedResponseError {
    pub(crate) fn is_limit_exceeded(&self) -> bool {
        matches!(
            self,
            Self::DeclaredLengthExceeded { .. } | Self::StreamedLengthExceeded { .. }
        )
    }
}

pub(crate) fn upstream_discovery_max_bytes() -> usize {
    upstream_discovery_max_bytes_from_raw(
        std::env::var(UPSTREAM_DISCOVERY_MAX_BYTES_ENV)
            .ok()
            .as_deref(),
    )
}

fn upstream_discovery_max_bytes_from_raw(raw: Option<&str>) -> usize {
    raw.map(str::trim)
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_UPSTREAM_DISCOVERY_MAX_BYTES)
}

pub(crate) async fn read_upstream_discovery_body(
    response: reqwest::Response,
) -> Result<Bytes, BoundedResponseError> {
    read_response_body_with_limit(response, upstream_discovery_max_bytes()).await
}

pub(crate) async fn read_response_body_with_limit(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Bytes, BoundedResponseError> {
    read_body(response, max_bytes, None).await
}

/// Like [`read_response_body_with_limit`], but fails when no chunk arrives within
/// `idle` of the previous one (or of the call, for the first chunk).
pub(crate) async fn read_response_body_with_limit_and_idle(
    response: reqwest::Response,
    max_bytes: usize,
    idle: std::time::Duration,
) -> Result<Bytes, BoundedResponseError> {
    read_body(response, max_bytes, Some(idle)).await
}

async fn read_body(
    response: reqwest::Response,
    max_bytes: usize,
    idle: Option<std::time::Duration>,
) -> Result<Bytes, BoundedResponseError> {
    let max_bytes_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if let Some(content_length) = response.content_length()
        && content_length > max_bytes_u64
    {
        return Err(BoundedResponseError::DeclaredLengthExceeded {
            content_length,
            max_bytes,
        });
    }

    let mut body = BytesMut::new();
    let mut stream = response.bytes_stream();
    loop {
        let next = match idle {
            Some(idle) => tokio::time::timeout(idle, stream.next())
                .await
                .map_err(|_| BoundedResponseError::Idle {
                    idle_ms: idle.as_millis(),
                })?,
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|source| BoundedResponseError::BodyRead { source })?;
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            return Err(BoundedResponseError::StreamedLengthExceeded { max_bytes });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn raw_response_server(response_head_and_body: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind raw response server");
        let address = listener.local_addr().expect("raw response address");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(response_head_and_body).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{address}/models")
    }

    async fn get_response(url: String) -> reqwest::Response {
        crate::node_config::ensure_rustls_crypto_provider()
            .expect("install Rustls crypto provider");
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build test HTTP client")
            .get(url)
            .send()
            .await
            .expect("response headers")
    }

    #[test]
    fn discovery_limit_parser_requires_positive_base_ten_integer() {
        assert_eq!(upstream_discovery_max_bytes_from_raw(Some("32")), 32);
        assert_eq!(upstream_discovery_max_bytes_from_raw(Some(" 64 ")), 64);
        for raw in ["", "0", "-1", "+1", "1.0", "invalid"] {
            assert_eq!(
                upstream_discovery_max_bytes_from_raw(Some(raw)),
                DEFAULT_UPSTREAM_DISCOVERY_MAX_BYTES
            );
        }
        assert_eq!(
            upstream_discovery_max_bytes_from_raw(Some(
                "999999999999999999999999999999999999999999999999"
            )),
            DEFAULT_UPSTREAM_DISCOVERY_MAX_BYTES
        );
        assert_eq!(
            upstream_discovery_max_bytes_from_raw(None),
            DEFAULT_UPSTREAM_DISCOVERY_MAX_BYTES
        );
    }

    #[tokio::test]
    async fn declared_content_length_over_limit_is_rejected() {
        let url = raw_response_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n123456789",
        )
        .await;
        let response = get_response(url).await;

        let error = read_response_body_with_limit(response, 8)
            .await
            .expect_err("declared body must be rejected");
        assert!(matches!(
            error,
            BoundedResponseError::DeclaredLengthExceeded {
                content_length: 9,
                max_bytes: 8
            }
        ));
    }

    #[tokio::test]
    async fn chunked_body_over_limit_is_rejected_while_streaming() {
        let url = raw_response_server(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\n1234\r\n5\r\n56789\r\n0\r\n\r\n",
        )
        .await;
        let response = get_response(url).await;
        assert_eq!(response.content_length(), None);

        let error = read_response_body_with_limit(response, 8)
            .await
            .expect_err("streamed body must be rejected");
        assert!(matches!(
            error,
            BoundedResponseError::StreamedLengthExceeded { max_bytes: 8 }
        ));
    }

    #[tokio::test]
    async fn body_equal_to_limit_is_accepted() {
        let url = raw_response_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678",
        )
        .await;
        let response = get_response(url).await;

        let body = read_response_body_with_limit(response, 8)
            .await
            .expect("body at limit is valid");
        assert_eq!(&body[..], b"12345678");
    }
}
