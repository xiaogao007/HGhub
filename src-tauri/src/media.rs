//! 媒体层：封面解密 + HLS 代理。
//!
//! 站点封面是 AES-128-CBC 密文，m3u8 内部分片/key 又可能被 WebView 的 CORS 拦住，
//! 所以图片与视频统一走本地自定义协议 `hg://localhost/...`。

use crate::net::{fetch_bytes, fetch_bytes_with_headers};
use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes128;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use once_cell::sync::Lazy;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tauri::http::{Response, StatusCode};

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CdnConfig {
    m3u8_enabled: bool,
    other_enabled: bool,
    auth_key: String,
    auth_value: String,
    app: String,
}

struct MediaContext {
    config: CdnConfig,
    headers: reqwest::header::HeaderMap,
    origins: HashSet<String>,
    created: std::time::Instant,
}

impl MediaContext {
    fn headers_for(&self, target: &str) -> Option<reqwest::header::HeaderMap> {
        let url = url::Url::parse(target).ok()?;
        if !self.origins.contains(&url.origin().ascii_serialization()) {
            return None;
        }
        let path = url.path().to_ascii_lowercase();
        let enabled = (self.config.m3u8_enabled && path.ends_with(".m3u8"))
            || (self.config.other_enabled
                && [".ts", ".m4s", ".key"]
                    .iter()
                    .any(|ext| path.ends_with(ext)));
        (enabled && !self.headers.is_empty()).then(|| self.headers.clone())
    }
}

static MEDIA_CONTEXTS: Lazy<Mutex<HashMap<String, Arc<MediaContext>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 凭据只留在 Rust 内存，前端仅收到不含凭据的本地上下文编号。
pub fn register_playback(main: &str, hevc: &str, cdn: serde_json::Value) -> Result<String, String> {
    let config: CdnConfig = serde_json::from_value(if cdn.is_null() {
        serde_json::json!({})
    } else {
        cdn
    })
    .map_err(|_| "播放 CDN 配置无效")?;
    let mut origins = HashSet::new();
    for value in [main, hevc].into_iter().filter(|s| !s.is_empty()) {
        let url = url::Url::parse(value).map_err(|_| "播放地址无效")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("播放地址无效".into());
        }
        origins.insert(url.origin().ascii_serialization());
    }
    let mut headers = reqwest::header::HeaderMap::new();
    if config.m3u8_enabled || config.other_enabled {
        use reqwest::header::{HeaderName, HeaderValue};
        let key = HeaderName::from_bytes(config.auth_key.as_bytes())
            .map_err(|_| "播放 CDN 请求头无效")?;
        if matches!(key.as_str(), "host" | "cookie" | "referer" | "x-cdn-app")
            || config.auth_value.is_empty()
            || config.app.is_empty()
        {
            return Err("播放 CDN 凭据无效".into());
        }
        headers.insert(
            key,
            HeaderValue::from_str(&config.auth_value).map_err(|_| "播放 CDN 凭据无效")?,
        );
        headers.insert(
            "x-cdn-app",
            HeaderValue::from_str(&config.app).map_err(|_| "播放 CDN 应用标识无效")?,
        );
    }
    let mut random = [0u8; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| "播放上下文创建失败")?;
    let id = URL_SAFE_NO_PAD.encode(random);
    let mut contexts = MEDIA_CONTEXTS.lock().map_err(|_| "播放上下文不可用")?;
    if contexts.len() >= PL_CACHE_MAX {
        if let Some(oldest) = contexts
            .iter()
            .min_by_key(|(_, context)| context.created)
            .map(|(key, _)| key.clone())
        {
            contexts.remove(&oldest);
        }
    }
    contexts.insert(
        id.clone(),
        Arc::new(MediaContext {
            config,
            headers,
            origins,
            created: std::time::Instant::now(),
        }),
    );
    Ok(format!("http://hg.localhost/hls?u={}&c={id}", enc(main)))
}

const IMG_KEY: &[u8] = b"f5d965df75336270";
const IMG_IV: &[u8] = b"97b60394abc2fbe1";

static IMG_CACHE: Lazy<Mutex<HashMap<String, Arc<Vec<u8>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static PL_CACHE: Lazy<Mutex<HashMap<String, Arc<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

const IMG_CACHE_MAX: usize = 240;
const PL_CACHE_MAX: usize = 60;

fn cache_put<V: Clone>(map: &Mutex<HashMap<String, V>>, key: String, val: V, max: usize) {
    let mut m = map.lock().unwrap();
    if m.len() >= max {
        if let Some(k) = m.keys().next().cloned() {
            m.remove(&k);
        }
    }
    m.insert(key, val);
}

/* ───────────────────────── AES ───────────────────────── */

fn aes_cbc_decrypt(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let cipher = Aes128::new_from_slice(IMG_KEY).ok()?;
    let mut prev = [0u8; 16];
    prev.copy_from_slice(IMG_IV);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut ct = [0u8; 16];
        ct.copy_from_slice(chunk);
        let mut block = ct;
        cipher.decrypt_block((&mut block).into());
        for i in 0..16 {
            out.push(block[i] ^ prev[i]);
        }
        prev = ct;
    }
    Some(out)
}

fn strip_padding(mut buf: Vec<u8>) -> Vec<u8> {
    let pad = *buf.last().unwrap_or(&0) as usize;
    if pad > 0 && pad <= 16 && buf.len() >= pad {
        if buf[buf.len() - pad..].iter().all(|b| *b as usize == pad) {
            buf.truncate(buf.len() - pad);
        }
    }
    buf
}

fn image_kind(buf: &[u8]) -> Option<&'static str> {
    if buf.len() < 12 {
        return None;
    }
    if buf[0] == 0xFF && buf[1] == 0xD8 {
        return Some("image/jpeg");
    }
    if buf.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        return Some("image/png");
    }
    if buf.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    if buf.starts_with(b"RIFF") && &buf[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// 取封面：命中缓存直接返回，否则下载并按需解密。
pub async fn load_image(url: &str) -> Result<(String, Arc<Vec<u8>>), String> {
    if let Some(hit) = IMG_CACHE.lock().unwrap().get(url).cloned() {
        let kind = image_kind(&hit).unwrap_or("image/jpeg");
        return Ok((kind.to_string(), hit));
    }
    let (_, bytes) = fetch_bytes(url).await?;
    let plain = if image_kind(&bytes).is_some() {
        bytes
    } else {
        let dec = aes_cbc_decrypt(&bytes).ok_or_else(|| "封面解密失败".to_string())?;
        strip_padding(dec)
    };
    let kind = image_kind(&plain).unwrap_or("image/jpeg").to_string();
    let arc = Arc::new(plain);
    cache_put(&IMG_CACHE, url.to_string(), arc.clone(), IMG_CACHE_MAX);
    Ok((kind, arc))
}

/* ───────────────────────── HLS ───────────────────────── */

fn enc(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

fn dec(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().to_string()
}

fn absolutize(base: &str, target: &str) -> String {
    if target.starts_with("http://") || target.starts_with("https://") {
        return target.to_string();
    }
    if let Ok(b) = url::Url::parse(base) {
        if let Ok(j) = b.join(target) {
            return j.to_string();
        }
    }
    target.to_string()
}

fn local_media_url(prefix: &str, url: &str, context: Option<&str>) -> String {
    let playlist = url::Url::parse(url)
        .ok()
        .is_some_and(|u| u.path().to_ascii_lowercase().ends_with(".m3u8"));
    let route = if playlist { "hls" } else { "seg" };
    let context = context.map(|id| format!("&c={id}")).unwrap_or_default();
    format!("{prefix}/{route}?u={}{context}", enc(url))
}

fn rewrite_uri_attr(line: &str, prefix: &str, context: Option<&str>) -> String {
    // #EXT-X-KEY:...URI="xxx" / #EXT-X-MAP:URI="xxx"
    if let Some(start) = line.find("URI=\"") {
        let rest = &line[start + 5..];
        if let Some(end) = rest.find('"') {
            let uri = &rest[..end];
            let new = local_media_url(prefix, uri, context);
            return format!("{}{}{}", &line[..start + 5], new, &rest[end..]);
        }
    }
    line.to_string()
}

/// 取 m3u8 并把 key / 分片地址改写到本地协议。
async fn load_playlist(
    url: &str,
    prefix: &str,
    context_id: Option<&str>,
    context: Option<&MediaContext>,
) -> Result<Arc<String>, String> {
    let cache_key = format!("{prefix}|{}|{url}", context_id.unwrap_or(""));
    if let Some(hit) = PL_CACHE.lock().unwrap().get(&cache_key).cloned() {
        return Ok(hit);
    }
    let (_, bytes) =
        fetch_bytes_with_headers(url, context.and_then(|c| c.headers_for(url))).await?;
    let text = String::from_utf8_lossy(&bytes).to_string();

    let mut out = String::with_capacity(text.len() + 512);
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            out.push('\n');
            continue;
        }
        if line.starts_with('#') {
            if line.starts_with("#EXT-X-KEY")
                || line.starts_with("#EXT-X-MAP")
                || line.starts_with("#EXT-X-MEDIA:")
                || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
            {
                // 先把相对 URI 绝对化再改写
                let absolute = absolutize_uri_in_attr(url, line);
                out.push_str(&rewrite_uri_attr(&absolute, prefix, context_id));
            } else {
                out.push_str(line);
            }
        } else {
            let absolute = absolutize(url, line);
            out.push_str(&local_media_url(prefix, &absolute, context_id));
        }
        out.push('\n');
    }

    let arc = Arc::new(out);
    cache_put(&PL_CACHE, cache_key, arc.clone(), PL_CACHE_MAX);
    Ok(arc)
}

fn absolutize_uri_in_attr(base: &str, line: &str) -> String {
    if let Some(start) = line.find("URI=\"") {
        let rest = &line[start + 5..];
        if let Some(end) = rest.find('"') {
            let uri = &rest[..end];
            let abs = absolutize(base, uri);
            return format!("{}{}{}", &line[..start + 5], abs, &rest[end..]);
        }
    }
    line.to_string()
}

/// 清空封面 / 播放列表内存缓存
pub fn clear_caches() {
    if let Ok(mut g) = IMG_CACHE.lock() {
        g.clear();
    }
    if let Ok(mut g) = PL_CACHE.lock() {
        g.clear();
    }
}

/* ───────────────────────── 协议响应 ───────────────────────── */

fn respond(status: u16, ctype: &str, body: Vec<u8>, cache: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::OK))
        .header("Content-Type", ctype)
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", cache)
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 统一入口：`hg://localhost/img?u=` / `hls?u=` / `seg?u=`
pub async fn handle(path: &str, query: &str, scheme: &str) -> Response<Vec<u8>> {
    let target = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("u="))
        .map(dec)
        .unwrap_or_default();

    if target.is_empty() {
        return respond(400, "text/plain", b"missing u".to_vec(), "no-store");
    }

    let prefix = format!("{scheme}://hg.localhost");
    let context_id = query.split('&').find_map(|part| part.strip_prefix("c="));
    let context = context_id.and_then(|id| MEDIA_CONTEXTS.lock().ok()?.get(id).cloned());
    if context_id.is_some() && context.is_none() {
        return respond(
            410,
            "text/plain",
            b"playback context expired; refresh playback".to_vec(),
            "no-store",
        );
    }

    match path {
        "/img" => match load_image(&target).await {
            Ok((kind, bytes)) => {
                respond(200, &kind, bytes.as_ref().clone(), "public, max-age=86400")
            }
            Err(e) => respond(502, "text/plain", e.into_bytes(), "no-store"),
        },
        "/hls" => match load_playlist(&target, &prefix, context_id, context.as_deref()).await {
            Ok(text) => respond(
                200,
                "application/vnd.apple.mpegurl",
                text.as_bytes().to_vec(),
                "no-store",
            ),
            Err(e) => respond(502, "text/plain", e.into_bytes(), "no-store"),
        },
        "/seg" => match fetch_bytes_with_headers(
            &target,
            context.as_ref().and_then(|c| c.headers_for(&target)),
        )
        .await
        {
            Ok((ctype, bytes)) => respond(
                200,
                ctype.as_deref().unwrap_or("application/octet-stream"),
                bytes,
                "public, max-age=3600",
            ),
            Err(e) => respond(502, "text/plain", e.into_bytes(), "no-store"),
        },
        _ => respond(404, "text/plain", b"not found".to_vec(), "no-store"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cdn_config(secret: &str) -> serde_json::Value {
        serde_json::json!({ "m3u8Enabled": true, "otherEnabled": false,
            "authKey": "x-fixture-auth", "authValue": secret, "app": "fixture-app" })
    }

    async fn local_request(url: &str) -> Response<Vec<u8>> {
        let parsed = url::Url::parse(url).unwrap();
        handle(parsed.path(), parsed.query().unwrap_or(""), "http").await
    }

    #[tokio::test]
    async fn cdn_headers_and_context_survive_nested_playlists_without_leaking_to_segments() {
        let server = crate::edge_crypto::tests::MockServer::start();
        let upstream = format!("{}/fixture/master.m3u8", server.1);
        let proxy = register_playback(&upstream, "", cdn_config("fixture-secret")).unwrap();
        assert!(!proxy.contains("fixture-secret"));
        let master = local_request(&proxy).await;
        assert_eq!(master.status(), 200);
        let master_text = String::from_utf8(master.body().clone()).unwrap();
        let variant = master_text
            .lines()
            .find(|line| !line.starts_with('#'))
            .unwrap();
        assert!(variant.contains("/hls?u=") && variant.contains("&c="));
        let playlist = local_request(variant).await;
        assert_eq!(playlist.status(), 200);
        let text = String::from_utf8(playlist.body().clone()).unwrap();
        let key_line = text
            .lines()
            .find(|line| line.starts_with("#EXT-X-KEY"))
            .unwrap();
        let key_url = key_line
            .split("URI=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let key = local_request(key_url).await;
        assert_eq!(key.status(), 200);
        assert_eq!(key.body().len(), 16);
        let segment_url = text.lines().find(|line| !line.starts_with('#')).unwrap();
        assert_eq!(local_request(segment_url).await.status(), 200);

        let invalid = register_playback(&upstream, "", cdn_config("expired-secret")).unwrap();
        assert_eq!(
            local_request(&invalid).await.status(),
            502,
            "playlist cache must be scoped to credentials"
        );
        let redirect = register_playback(
            &format!("{}/fixture/redirect.m3u8", server.1),
            "",
            cdn_config("fixture-secret"),
        )
        .unwrap();
        assert_eq!(
            local_request(&redirect).await.status(),
            502,
            "credentials must not follow a cross-origin redirect"
        );
    }

    #[test]
    fn cdn_headers_are_limited_to_registered_origins_and_enabled_extensions() {
        let proxy = register_playback(
            "https://media.example.test/main.m3u8",
            "",
            cdn_config("fixture-secret"),
        )
        .unwrap();
        let id = proxy.split("&c=").nth(1).unwrap();
        let context = MEDIA_CONTEXTS.lock().unwrap().get(id).unwrap().clone();
        assert!(context
            .headers_for("https://media.example.test/variant.m3u8")
            .is_some());
        assert!(context
            .headers_for("https://other.example.test/variant.m3u8")
            .is_none());
        assert!(context
            .headers_for("http://media.example.test/variant.m3u8")
            .is_none());
        assert!(context
            .headers_for("https://media.example.test/segment.ts")
            .is_none());
        assert!(register_playback(
            "file:///private/video.m3u8",
            "",
            cdn_config("fixture-secret")
        )
        .is_err());
    }

    #[tokio::test]
    #[ignore = "requires live upstream media"]
    async fn live_play_chain() {
        use crate::net::edge;
        let play = edge("GET", "/api/videos/117/play?ep=3&page_texts=0", None, None)
            .await
            .expect("play api");
        let url = play["video_url"].as_str().expect("video_url").to_string();
        println!("play url: {}", &url[..url.len().min(90)]);
        let (_, bytes) = fetch_bytes(&url).await.expect("fetch playlist");
        let text = String::from_utf8_lossy(&bytes);
        println!(
            "raw playlist len={} head={}",
            text.len(),
            &text[..text.len().min(160)]
        );
        let q = format!("u={}", enc(&url));
        let resp = handle("/hls", &q, "http").await;
        let body = String::from_utf8_lossy(resp.body()).to_string();
        println!("proxied status={} len={}", resp.status(), body.len());
        assert!(body.contains("#EXTM3U"));
        // 取一个分片验证
        let seg_line = body
            .lines()
            .find(|l| !l.starts_with('#') && l.contains("/seg?u="))
            .expect("seg line")
            .to_string();
        println!("seg line: {}", &seg_line[..seg_line.len().min(120)]);
        let seg_q = seg_line.split("?u=").nth(1).unwrap();
        let seg = handle("/seg", &format!("u={seg_q}"), "http").await;
        println!("segment status={} len={}", seg.status(), seg.body().len());
        assert_eq!(seg.status(), 200);
        assert!(seg.body().len() > 1000);
    }

    #[tokio::test]
    #[ignore = "requires a fresh upstream signed URL"]
    async fn playlist_rewrite_works() {
        let m3u8 = "https://yd-lphu-hls.hzjvlo.cn/videos5/247f3c9862e8e1537ade0b20c45ac962/247f3c9862e8e1537ade0b20c45ac962.m3u8?auth_key=1793700503-0-0-b9575bb11258cf40cd5499c052fc5820&v=3&time=0&via=hgdj&via_bm=dx";
        let q = format!("u={}", enc(m3u8));
        let resp = handle("/hls", &q, "http").await;
        let body = String::from_utf8_lossy(resp.body()).to_string();
        println!("status={} len={}", resp.status(), body.len());
        println!("--- head ---\n{}", &body[..body.len().min(600)]);
        assert!(body.contains("#EXTM3U"), "not a playlist: {body}");
        assert!(
            body.contains("hg.localhost/seg?u="),
            "segments not rewritten"
        );
    }

    #[tokio::test]
    #[ignore = "requires a fresh upstream signed URL"]
    async fn cover_decrypt_works() {
        let url = "https://pic.drleao.cn/upload_01/upload/20261029/2026102918545028336.jpg?auth_key=1793707703-0-0-1cbd9389b0b69141dd741f16bb71397a";
        match load_image(url).await {
            Ok((kind, bytes)) => {
                println!("kind={kind} len={}", bytes.len());
                assert!(kind.starts_with("image/"));
            }
            Err(e) => panic!("image failed: {e}"),
        }
    }
}
