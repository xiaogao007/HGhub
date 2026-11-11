//! 网络层：与上游站点对话。
//!
//! 列表等保留原边缘信封；播放业务使用独立的加密通道。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::time::Duration;

pub const BASE: &str = "https://huangguoai.com";

pub const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .gzip(true)
        .brotli(true)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .expect("build http client")
});

// 自定义 CDN 凭据不能随重定向泄露到另一个源。
static CDN_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many media redirects");
            }
            if attempt
                .previous()
                .last()
                .is_some_and(|previous| previous.origin() == attempt.url().origin())
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .expect("build CDN http client")
});

/// 站点偶发 TLS 抖动（实测），统一重试。
async fn with_retry<F, Fut, T>(mut f: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last = String::from("network error");
    for attempt in 0..3 {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                last = e;
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(350 * (attempt as u64 + 1))).await;
                }
            }
        }
    }
    Err(last)
}

/// 业务请求：走边缘信封。
pub async fn edge(
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> Result<Value, String> {
    let (encoded, ctype) = match &body {
        Some(v) => (
            B64.encode(serde_json::to_vec(v).map_err(|e| e.to_string())?),
            "application/json",
        ),
        None => (String::new(), ""),
    };
    let envelope = json!({
        "method": method,
        "path": path,
        "body": encoded,
        "content_type": ctype,
    });

    let url = format!("{BASE}/api/edge/request");
    let token = token.map(|s| s.to_string());

    // 播放地址已迁移到独立加密通道，旧桥接接口会成功返回空地址。
    let route = path.split('?').next().unwrap_or(path);
    let segments: Vec<_> = route.trim_start_matches('/').split('/').collect();
    let is_playback = segments.len() >= 4
        && segments[0] == "api"
        && segments[1] == "videos"
        && segments[2].parse::<u64>().is_ok()
        && ((method == "GET" && segments.len() == 4 && segments[3] == "play")
            || (method == "POST"
                && segments.len() == 6
                && segments[3] == "episodes"
                && segments[4].parse::<u32>().is_ok()
                && segments[5] == "playback"));
    let value = if is_playback {
        crate::edge_crypto::PLAYBACK
            .call(&CLIENT, BASE, &envelope, token.as_deref())
            .await?
    } else {
        with_retry(|| {
            let envelope = envelope.clone();
            let token = token.clone();
            let url = url.clone();
            async move {
                let mut req = CLIENT
                    .post(&url)
                    .header("Accept", "application/json")
                    .header("Referer", format!("{BASE}/"))
                    .json(&envelope);
                if let Some(t) = token.as_deref() {
                    if !t.is_empty() {
                        req = req.header("Authorization", format!("Bearer {t}"));
                    }
                }
                let resp = req.send().await.map_err(|e| e.to_string())?;
                let status = resp.status();
                let text = resp.text().await.map_err(|e| e.to_string())?;
                if status.as_u16() == 401 || status.as_u16() == 403 {
                    return Err("401".to_string());
                }
                if !status.is_success() {
                    return Err(format!("HTTP {status}: {}", &text[..text.len().min(160)]));
                }
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                serde_json::from_str::<Value>(&text)
                    .map_err(|e| format!("{e}: {}", &text[..text.len().min(160)]))
            }
        })
        .await?
    };

    if value.is_null() {
        return Ok(Value::Null);
    }

    if value.get("status").and_then(|v| v.as_i64()) == Some(1) {
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(value
            .get("msg")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("接口返回失败")
            .to_string())
    }
}

/// 拉原始字节（图片 / m3u8 / 分片）。
pub async fn fetch_bytes(url: &str) -> Result<(Option<String>, Vec<u8>), String> {
    fetch_bytes_with_headers(url, None).await
}

pub async fn fetch_bytes_with_headers(
    url: &str,
    headers: Option<reqwest::header::HeaderMap>,
) -> Result<(Option<String>, Vec<u8>), String> {
    let url = url.to_string();
    with_retry(|| {
        let url = url.clone();
        let headers = headers.clone();
        async move {
            let mut request = if headers.is_some() {
                &*CDN_CLIENT
            } else {
                &*CLIENT
            }
            .get(&url)
            .header("Referer", format!("{BASE}/"));
            if let Some(headers) = headers {
                request = request.headers(headers);
            }
            let resp = request.send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            let ctype = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let bytes = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
            Ok((ctype, bytes))
        }
    })
    .await
}
