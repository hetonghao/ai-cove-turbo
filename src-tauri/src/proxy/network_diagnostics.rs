use std::{error::Error as StdError, io};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FailureStage {
    Dns,
    Tls,
    Connect,
    Write,
    Read,
    Timeout,
    Reset,
    UpstreamHttp,
    Unknown,
}

pub(crate) async fn classify_request_error(
    error: &reqwest::Error,
    target: &url::Url,
) -> FailureStage {
    if error.is_timeout() {
        return FailureStage::Timeout;
    }
    if source_io_kind(error).is_some_and(is_reset_kind) {
        return FailureStage::Reset;
    }
    if source_is::<rustls::Error>(error) {
        return FailureStage::Tls;
    }
    if error.is_connect() {
        return match dns_lookup(target).await {
            Some(false) => FailureStage::Dns,
            Some(true) => FailureStage::Connect,
            None => FailureStage::Unknown,
        };
    }
    classify_typed_error(error, false).unwrap_or(FailureStage::Unknown)
}

pub(crate) fn classify_stream_error(error: &reqwest::Error) -> FailureStage {
    classify_typed_error(error, true).unwrap_or(FailureStage::Read)
}

pub(crate) const fn failure_reason(stage: FailureStage) -> &'static str {
    match stage {
        FailureStage::Dns => "upstream request failed (stage=dns)",
        FailureStage::Tls => "upstream request failed (stage=tls)",
        FailureStage::Connect => "upstream request failed (stage=connect)",
        FailureStage::Write => "upstream request failed (stage=write)",
        FailureStage::Read => "upstream request failed (stage=read)",
        FailureStage::Timeout => "upstream request failed (stage=timeout)",
        FailureStage::Reset => "upstream request failed (stage=reset)",
        FailureStage::UpstreamHttp => "upstream request failed (stage=upstreamHttp)",
        FailureStage::Unknown => "upstream request failed (stage=unknown)",
    }
}

pub(crate) fn failure_stage_from_reason(reason: Option<&str>) -> Option<FailureStage> {
    let reason = reason?;
    let stage = reason
        .strip_prefix("upstream request failed (stage=")
        .and_then(|value| value.strip_suffix(')'))
        .or_else(|| {
            (reason == "HTTP response stream failed"
                || reason == "HTTP stream ended before terminal response event")
                .then_some("read")
        })
        .or_else(|| {
            reason
                .starts_with("HTTP upstream returned status ")
                .then_some("upstreamHttp")
        })?;
    parse_stage(stage)
}

fn classify_typed_error(error: &reqwest::Error, stream: bool) -> Option<FailureStage> {
    if error.is_timeout() {
        return Some(FailureStage::Timeout);
    }
    if source_io_kind(error).is_some_and(is_reset_kind) {
        return Some(FailureStage::Reset);
    }
    if source_is::<rustls::Error>(error) {
        return Some(FailureStage::Tls);
    }
    if stream {
        return Some(FailureStage::Read);
    }
    if error.is_body() || error.is_request() {
        return Some(FailureStage::Write);
    }
    if error.is_decode() {
        return Some(FailureStage::Read);
    }
    None
}

fn source_is<T: std::error::Error + 'static>(error: &reqwest::Error) -> bool {
    let mut source = error.source();
    while let Some(current) = source {
        if current.is::<T>() {
            return true;
        }
        source = current.source();
    }
    false
}

fn source_io_kind(error: &reqwest::Error) -> Option<io::ErrorKind> {
    let mut source = error.source();
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<io::Error>() {
            return Some(io_error.kind());
        }
        source = current.source();
    }
    None
}

async fn dns_lookup(target: &url::Url) -> Option<bool> {
    let host = target.host_str()?;
    let port = target.port_or_known_default()?;
    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .ok()
    .map(|result| result.is_ok())
}

const fn is_reset_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof
    )
}

fn parse_stage(stage: &str) -> Option<FailureStage> {
    Some(match stage {
        "dns" => FailureStage::Dns,
        "tls" => FailureStage::Tls,
        "connect" => FailureStage::Connect,
        "write" => FailureStage::Write,
        "read" => FailureStage::Read,
        "timeout" => FailureStage::Timeout,
        "reset" => FailureStage::Reset,
        "upstreamHttp" => FailureStage::UpstreamHttp,
        "unknown" => FailureStage::Unknown,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_stream_failures_as_read_without_raw_error_text() {
        assert_eq!(
            failure_reason(FailureStage::Read),
            "upstream request failed (stage=read)"
        );
        assert_eq!(
            failure_stage_from_reason(Some("HTTP upstream returned status 503")),
            Some(FailureStage::UpstreamHttp)
        );
    }

    #[tokio::test]
    async fn classifies_local_connection_failures_by_error_kind() {
        let client = reqwest::Client::new();
        let dns_target = url::Url::parse("http://network-diagnostics.invalid/v1").expect("URL");
        let dns_error = client
            .get(dns_target.as_str())
            .send()
            .await
            .expect_err("DNS must fail");
        assert_eq!(
            classify_request_error(&dns_error, &dns_target).await,
            FailureStage::Dns
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("local address");
        drop(listener);
        let connect_target = url::Url::parse(&format!("http://{address}/v1")).expect("URL");
        let connect_error = client
            .get(connect_target.as_str())
            .send()
            .await
            .expect_err("connection must fail");
        assert_eq!(
            classify_request_error(&connect_error, &connect_target).await,
            FailureStage::Connect
        );
    }
}
