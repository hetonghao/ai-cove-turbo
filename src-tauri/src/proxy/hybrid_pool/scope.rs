use std::{fmt, hash::BuildHasher};

use axum::http::HeaderMap;
use url::Url;

use super::super::{hop_by_hop_headers, private_websocket};

#[derive(Clone, Eq, Hash, PartialEq)]
pub(in crate::proxy) struct HybridScope {
    pub(super) target: String,
    pub(super) headers: Vec<(String, Vec<u8>)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScopeFingerprint(u64);

impl HybridScope {
    pub(in crate::proxy) fn new(target: &Url, client_headers: &HeaderMap) -> Self {
        let connection_headers = blank_connection_headers(client_headers);
        let mut headers = connection_headers
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect::<Vec<_>>();
        headers.sort_unstable();
        Self {
            target: target.as_str().to_owned(),
            headers,
        }
    }

    pub(super) fn fingerprint<S: BuildHasher>(&self, state: &S) -> ScopeFingerprint {
        ScopeFingerprint(state.hash_one(self))
    }
}

impl fmt::Display for ScopeFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "G{:016X}", self.0)
    }
}

pub(super) fn blank_connection_headers(client_headers: &HeaderMap) -> HeaderMap {
    let hop_by_hop = hop_by_hop_headers(client_headers);
    client_headers
        .iter()
        .filter(|(name, _)| {
            !hop_by_hop.contains(*name)
                && !private_websocket::is_client_handshake_header(name)
                && !matches!(
                    name.as_str(),
                    "session-id"
                        | "thread-id"
                        | "x-client-request-id"
                        | "x-codex-installation-id"
                        | "x-codex-window-id"
                        | "x-codex-turn-metadata"
                        | "x-codex-parent-thread-id"
                        | "x-openai-subagent"
                )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

impl fmt::Debug for HybridScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HybridScope")
            .field("header_count", &self.headers.len())
            .finish_non_exhaustive()
    }
}
