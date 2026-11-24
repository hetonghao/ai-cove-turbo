use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

mod codec;
#[cfg(test)]
mod tests;
pub(crate) const HEADER: &str = "x-ai-cove-image-processing";

pub(crate) fn may_have_inline_image(body: &[u8]) -> bool {
    static DATA: OnceLock<memchr::memmem::Finder<'static>> = OnceLock::new();
    static UNICODE: OnceLock<memchr::memmem::Finder<'static>> = OnceLock::new();
    DATA.get_or_init(|| memchr::memmem::Finder::new(b"data:"))
        .find(body)
        .is_some()
        || UNICODE
            .get_or_init(|| memchr::memmem::Finder::new(br"\u00"))
            .find(body)
            .is_some()
}

/// Keep our private metadata out of user-controlled generations, the HTTP fallback, and the connection pool.
pub(crate) fn ws_metadata(payload: Vec<u8>, marker: Option<&str>) -> Vec<u8> {
    if marker.is_none()
        && !payload
            .windows(HEADER.len())
            .any(|b| b == HEADER.as_bytes())
    {
        return payload;
    }
    let Ok(mut obj) = serde_json::from_slice::<Value>(&payload) else {
        return payload;
    };
    let Some(map) = obj.as_object_mut() else {
        return payload;
    };
    if let Some(value) = marker {
        let metadata = map
            .entry("client_metadata")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let Some(meta) = metadata.as_object_mut() else {
            return payload;
        };
        meta.insert(HEADER.to_owned(), Value::String(value.to_owned()));
    } else if !map
        .get_mut("client_metadata")
        .and_then(Value::as_object_mut)
        .is_some_and(|m| m.remove(HEADER).is_some())
    {
        return payload;
    }
    serde_json::to_vec(&obj).unwrap_or(payload)
}
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const WORK_BUDGET: Duration = Duration::from_secs(12);
const IDLE_TTL: Duration = Duration::from_secs(900);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    #[default]
    Lossless,
    Lossy,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Format {
    #[default]
    Auto,
    Jpeg,
    Png,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Settings {
    pub(crate) mode: Mode,
    pub(crate) min_bytes: u32,
    pub(crate) max_side: u32,
    pub(crate) quality: u8,
    pub(crate) format: Format,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Lossless,
            min_bytes: 0,
            max_side: 2048,
            quality: 85,
            format: Format::Auto,
        }
    }
}
impl Settings {
    pub(crate) fn valid(self) -> bool {
        (16..=16384).contains(&self.max_side) && (1..=100).contains(&self.quality)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImageDetail {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) before: usize,
    pub(crate) after: usize,
    pub(crate) cached: bool,
    pub(crate) reason: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Report {
    #[serde(default)]
    pub(crate) json_before: usize,
    #[serde(default)]
    pub(crate) json_after: usize,
    pub(crate) count: usize,
    pub(crate) changed: usize,
    pub(crate) hits: usize,
    pub(crate) before: usize,
    pub(crate) after: usize,
    pub(crate) elapsed_ms: u64,
    pub(crate) details: Vec<ImageDetail>,
    pub(crate) fallback: bool,
}
impl Report {
    pub(crate) fn marker(&self) -> Option<String> {
        if self.fallback || self.count == 0 {
            None
        } else if self.changed > 0 {
            Some(format!("v1/changed/{}", self.changed))
        } else {
            Some("v1/kept".to_owned())
        }
    }
}
#[derive(Clone, Debug)]
struct Encoded {
    mime: &'static str,
    bytes: Option<Arc<[u8]>>,
    reason: &'static str,
}
impl Encoded {
    fn kept(mime: &'static str, reason: &'static str) -> Self {
        Self {
            mime,
            bytes: None,
            reason,
        }
    }
}
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Key {
    scope: String,
    source: [u8; 32],
    mime: &'static str,
    mode: Mode,
    format: Format,
    side: u32,
    min_bytes: u32,
    quality: u8,
    webp: bool,
}
type Slot = Arc<OnceLock<Option<(Encoded, usize)>>>;
#[derive(Debug, Default)]
struct Cache {
    entries: HashMap<Key, (Instant, Slot, usize)>,
    bytes: usize,
    generation: u64,
}
impl Cache {
    fn get(&mut self, key: &Key, now: Instant) -> Slot {
        if let Some((last, slot, _)) = self.entries.get_mut(key) {
            if now.duration_since(*last) < IDLE_TTL {
                *last = now;
                return Arc::clone(slot);
            }
        }
        self.remove(key);
        let slot = Arc::new(OnceLock::new());
        self.entries
            .insert(key.clone(), (now, Arc::clone(&slot), 0));
        slot
    }
    fn remove(&mut self, key: &Key) {
        if let Some((_, _, n)) = self.entries.remove(key) {
            self.bytes -= n;
        }
    }
    fn finish(&mut self, key: &Key, slot: &Slot, value: Option<&Encoded>) {
        let Some((_, current, weight)) = self.entries.get_mut(key) else {
            return;
        };
        if !Arc::ptr_eq(current, slot) {
            return;
        }
        let Some(value) = value else {
            self.remove(key);
            return;
        };
        let n = value.bytes.as_ref().map_or(0, |b| b.len());
        self.bytes += n - *weight;
        *weight = n;
        while self.bytes > MAX_CACHE_BYTES || self.entries.len() > 512 {
            let Some(old) = self
                .entries
                .iter()
                .min_by_key(|(_, v)| v.0)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.remove(&old);
        }
    }
}
#[derive(Debug)]
pub(crate) struct Processor {
    settings: Mutex<Settings>,
    cache: Mutex<Cache>,
    workers: Arc<Semaphore>,
}
impl Default for Processor {
    fn default() -> Self {
        Self {
            settings: Mutex::default(),
            cache: Mutex::default(),
            workers: Arc::new(Semaphore::new(2)),
        }
    }
}
impl Processor {
    pub(crate) fn configure(&self, value: Settings) {
        *self
            .settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
    }
    #[cfg(test)]
    pub(crate) async fn hold_encoders_for_test(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::AcquireError> {
        self.workers.clone().acquire_many_owned(2).await
    }

    pub(crate) fn usage(&self) -> (usize, usize) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let stale: Vec<_> = cache
            .entries
            .iter()
            .filter(|(_, v)| now.duration_since(v.0) >= IDLE_TTL)
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            cache.remove(&k)
        }
        (cache.bytes, cache.entries.len())
    }
    pub(crate) fn clear(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *cache = Cache {
            generation: cache.generation.wrapping_add(1),
            ..Cache::default()
        };
    }
    pub(crate) async fn prepare(
        self: &Arc<Self>,
        body: Bytes,
        scope: String,
        webp: bool,
    ) -> (Bytes, Option<Report>) {
        let started = Instant::now();
        let fallback = || {
            Some(Report {
                fallback: true,
                ..Report::default()
            })
        };
        let Ok(Ok(permit)) = tokio::time::timeout(
            Duration::from_millis(250),
            self.workers.clone().acquire_owned(),
        )
        .await
        else {
            return (body, fallback());
        };
        let work = Arc::clone(self);
        let inp = body.clone();
        let cfg = *self
            .settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work.prepare_sync(&inp, &scope, cfg, webp, started)
        });
        let Ok(result) =
            tokio::time::timeout(WORK_BUDGET.saturating_sub(started.elapsed()), work).await
        else {
            return (body, fallback());
        };
        match result {
            Ok(Ok((replacement, Some(mut report)))) => {
                report.json_before = body.len();
                report.json_after = replacement.as_ref().map_or(body.len(), Bytes::len);
                report.elapsed_ms =
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                (replacement.unwrap_or(body), Some(report))
            }
            Ok(Ok((_, None))) => (body, None),
            _ => (body, fallback()),
        }
    }
    fn prepare_sync(
        &self,
        body: &[u8],
        scope: &str,
        settings: Settings,
        webp: bool,
        start: Instant,
    ) -> Result<(Option<Bytes>, Option<Report>), ()> {
        let generation = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation;
        let mut root: Value = serde_json::from_slice(body).map_err(|_| ())?;
        let mut report = Report::default();
        let mut cold = 0;
        let Some(items) = root.get_mut("input").and_then(Value::as_array_mut) else {
            return Ok((None, None));
        };
        for item in items {
            let ty = item.get("type").and_then(Value::as_str);
            let field = match ty {
                Some(
                    "function_call_output" | "custom_tool_call_output" | "local_shell_call_output",
                ) => "output",
                None | Some("message") => "content",
                _ => continue,
            };
            let Some(parts) = item.get_mut(field).and_then(Value::as_array_mut) else {
                continue;
            };
            for part in parts {
                if part.get("type").and_then(Value::as_str) != Some("input_image") {
                    continue;
                }
                let Some(url) = part.get("image_url").and_then(Value::as_str) else {
                    continue;
                };
                if !url.starts_with("data:") {
                    continue;
                }
                let (tag, data) = url.split_once(',').ok_or(())?;
                let name = tag
                    .strip_prefix("data:")
                    .and_then(|v| v.strip_suffix(";base64"))
                    .ok_or(())?;
                let mime = match name {
                    "image/png" => "image/png",
                    "image/jpeg" => "image/jpeg",
                    "image/webp" => "image/webp",
                    "image/gif" => "image/gif",
                    _ => return Err(()),
                };
                if data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
                    || report.count >= 256
                    || start.elapsed() > WORK_BUDGET
                {
                    return Err(());
                }
                let decode = || {
                    let raw = STANDARD.decode(data).map_err(|_| ())?;
                    if raw.len() > MAX_IMAGE_BYTES || raw.is_empty() {
                        return Err(());
                    }
                    Ok(raw)
                };
                let lossless = settings.mode == Mode::Lossless;
                let decoded_len = data.len() / 4 * 3;
                let padding = if data.ends_with("==") {
                    2
                } else {
                    usize::from(data.ends_with('='))
                };
                let (result, before, hit) = if lossless
                    && (decoded_len.saturating_sub(padding) < settings.min_bytes as usize
                        || matches!(mime, "image/jpeg" | "image/webp"))
                {
                    (Encoded::kept(mime, "policy"), decode()?.len(), false)
                } else {
                    let key = Key {
                        scope: scope.to_owned(),
                        // STANDARD accepts only canonical base64, so the decoded image has one cache key.
                        source: digest(&SHA256, data.as_bytes())
                            .as_ref()
                            .try_into()
                            .map_err(|_| ())?,
                        mime,
                        mode: settings.mode,
                        format: if lossless {
                            Format::Auto
                        } else {
                            settings.format
                        },
                        side: if lossless { 0 } else { settings.max_side },
                        min_bytes: if lossless { 0 } else { settings.min_bytes },
                        quality: if lossless { 0 } else { settings.quality },
                        webp,
                    };
                    let slot = {
                        let mut cache = self
                            .cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if cache.generation == generation {
                            cache.get(&key, Instant::now())
                        } else {
                            Arc::new(OnceLock::new())
                        }
                    };
                    let mut hit = true;
                    let value = slot.get_or_init(|| {
                        hit = false;
                        cold += 1;
                        if cold > 64 {
                            return None;
                        }
                        let raw = decode().ok()?;
                        let encoded = codec::encode(&raw, mime, settings, webp).ok()?;
                        Some((encoded, raw.len()))
                    });
                    self.cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .finish(&key, &slot, value.as_ref().map(|(encoded, _)| encoded));
                    let (encoded, before) = value.clone().ok_or(())?;
                    (encoded, before, hit)
                };
                if start.elapsed() > WORK_BUDGET {
                    return Err(());
                }
                let after = result.bytes.as_ref().map_or(before, |v| v.len());
                if let Some(encoded) = result.bytes {
                    part["image_url"] = Value::String(format!(
                        "data:{};base64,{}",
                        result.mime,
                        STANDARD.encode(encoded)
                    ));
                    report.changed += 1;
                }
                report.count += 1;
                report.hits += usize::from(hit);
                report.before += before;
                report.after += after;
                if report.details.len() < 16 {
                    report.details.push(ImageDetail {
                        from: mime.to_owned(),
                        to: result.mime.to_owned(),
                        before,
                        after,
                        cached: hit,
                        reason: result.reason.to_owned(),
                    });
                }
            }
        }
        if report.count == 0 {
            return Ok((None, None));
        }
        let output = if report.changed == 0 {
            None
        } else {
            Some(Bytes::from(serde_json::to_vec(&root).map_err(|_| ())?))
        };
        Ok((output, Some(report)))
    }
}
