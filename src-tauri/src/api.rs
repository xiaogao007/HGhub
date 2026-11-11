//! 业务命令：登录 / 列表 / 详情 / 播放 / 互动。

use crate::net::{edge, BASE};
use crate::session::SessionStore;
use serde_json::{json, Value};
use tauri::State;

pub struct Api {
    pub session: SessionStore,
}

impl Api {
    pub fn new() -> Self {
        Self {
            session: SessionStore::new(),
        }
    }

    fn token(&self) -> Option<String> {
        self.session.token()
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let token = self.token();
        edge("GET", path, None, token.as_deref()).await
    }

    async fn authed(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let token = self.token().ok_or_else(|| "请先登录".to_string())?;
        edge(method, path, body, Some(&token)).await
    }
}

/// 诊断用：把前端自检结果写到应用数据目录 debug.log
#[tauri::command]
pub async fn debug_log(app: tauri::AppHandle, msg: String) -> Result<(), String> {
    use tauri::Manager;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("debug.log");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", msg);
    }
    Ok(())
}

/* ───────────────────────── 会话 ───────────────────────── */

#[tauri::command]
pub async fn app_boot(state: State<'_, Api>) -> Result<Value, String> {
    Ok(json!({
        "user": state.session.user(),
        "logged_in": state.session.token().is_some(),
        "base": BASE,
    }))
}

#[tauri::command]
pub async fn api_login(
    state: State<'_, Api>,
    username: String,
    password: String,
) -> Result<Value, String> {
    let data = edge(
        "POST",
        "/api/account/loginByPassword",
        Some(json!({ "username": username, "password": password })),
        None,
    )
    .await?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "登录返回缺少 token".to_string())?
        .to_string();
    let user = data.get("user").cloned().unwrap_or(Value::Null);
    state.session.set(token, user.clone());
    Ok(json!({ "user": user }))
}

#[tauri::command]
pub async fn api_register(
    state: State<'_, Api>,
    username: String,
    password: String,
    email: String,
) -> Result<Value, String> {
    let mut body = json!({
        "username": username,
        "password": password,
        "confirm_password": password,
    });
    if !email.trim().is_empty() {
        body["email"] = json!(email);
    }
    let data = edge("POST", "/api/account/registerByPassword", Some(body), None).await?;
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "注册返回缺少 token".to_string())?
        .to_string();
    let user = data.get("user").cloned().unwrap_or(Value::Null);
    state.session.set(token, user.clone());
    Ok(json!({ "user": user }))
}

#[tauri::command]
pub async fn api_logout(state: State<'_, Api>) -> Result<Value, String> {
    let token = state.token();
    let _ = edge("POST", "/api/account/logout", None, token.as_deref()).await;
    state.session.clear();
    Ok(json!({ "ok": true }))
}

#[tauri::command]
pub async fn api_me(state: State<'_, Api>) -> Result<Value, String> {
    let me = state.get("/api/account/me").await?;
    if let Some(user) = state.session.token() {
        state.session.set(user, me.clone());
    }
    Ok(me)
}

/* ───────────────────────── 列表 ───────────────────────── */

#[tauri::command]
pub async fn api_feed(state: State<'_, Api>, page: u32, size: u32) -> Result<Value, String> {
    state
        .get(&format!("/api/videos?page={page}&size={size}"))
        .await
}

#[tauri::command]
pub async fn api_category(
    state: State<'_, Api>,
    cate: String,
    page: u32,
    size: u32,
) -> Result<Value, String> {
    state
        .get(&format!(
            "/api/videos/category/{cate}?page={page}&size={size}"
        ))
        .await
}

#[tauri::command]
pub async fn api_rank(state: State<'_, Api>, kind: String) -> Result<Value, String> {
    let kind = match kind.as_str() {
        "recommend" | "potential" => kind,
        _ => "hot".to_string(),
    };
    state.get(&format!("/api/ranks/{kind}")).await
}

#[tauri::command]
pub async fn api_search(
    state: State<'_, Api>,
    q: String,
    page: u32,
    size: u32,
) -> Result<Value, String> {
    let encoded: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
    state
        .get(&format!("/api/search?q={encoded}&page={page}&size={size}"))
        .await
}

#[tauri::command]
pub async fn api_posts(state: State<'_, Api>, page: u32, size: u32) -> Result<Value, String> {
    state
        .get(&format!("/api/posts?page={page}&size={size}"))
        .await
}

#[tauri::command]
pub async fn api_post(state: State<'_, Api>, id: i64) -> Result<Value, String> {
    state.get(&format!("/api/posts/{id}")).await
}

/* ───────────────────────── 详情 / 播放 ───────────────────────── */

#[tauri::command]
pub async fn api_video(state: State<'_, Api>, id: i64) -> Result<Value, String> {
    state.get(&format!("/api/videos/{id}")).await
}

#[tauri::command]
pub async fn api_play(state: State<'_, Api>, id: i64, ep: u32) -> Result<Value, String> {
    if id <= 0 || ep == 0 || ep > i32::MAX as u32 {
        return Err("无效的视频或集号".into());
    }
    let token = state.token();
    let data = edge(
        "POST",
        &format!("/api/videos/{id}/episodes/{ep}/playback"),
        Some(json!({})),
        token.as_deref(),
    )
    .await?;
    normalize_playback(data, id, ep)
}

fn normalize_playback(data: Value, id: i64, ep: u32) -> Result<Value, String> {
    let number = |value: &Value| {
        value
            .as_i64()
            .or_else(|| value.as_str()?.parse::<i64>().ok())
    };
    if data["manifest_method"] != "GET"
        || number(&data["id"]) != Some(id)
        || number(&data["ep"]) != Some(ep as i64)
    {
        return Err("播放接口返回的视频或集号不匹配".into());
    }
    let main = data["streams"]["main"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("播放接口未返回可用地址，请稍后重试")?;
    let hevc = data["streams"]["hevc"].as_str().unwrap_or("");
    let proxy_url = crate::media::register_playback(main, hevc, data["cdn"].clone())?;
    Ok(
        json!({ "id": id, "ep_num": ep, "duration": 0, "title": "", "video_url": main,
        "video_url_hevc": hevc, "proxy_url": proxy_url }),
    )
}

/* ───────────────────────── 评论 / 互动 ───────────────────────── */

#[tauri::command]
pub async fn api_comments(
    state: State<'_, Api>,
    id: i64,
    ep: u32,
    page: u32,
    size: u32,
) -> Result<Value, String> {
    state
        .get(&format!(
            "/api/videos/{id}/comments?ep={ep}&page={page}&size={size}"
        ))
        .await
}

#[tauri::command]
pub async fn api_comment_send(
    state: State<'_, Api>,
    id: i64,
    ep: u32,
    text: String,
    reply_id: Option<i64>,
) -> Result<Value, String> {
    let mut body = json!({ "text": text });
    let path = match reply_id {
        Some(rid) if rid > 0 => {
            body["reply_id"] = json!(rid);
            format!("/api/videos/{id}/comments")
        }
        _ => format!("/api/videos/{id}/comments?ep={ep}"),
    };
    state.authed("POST", &path, Some(body)).await
}

#[tauri::command]
pub async fn api_like(state: State<'_, Api>, id: i64, ep: u32, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    state
        .authed(method, &format!("/api/videos/{id}/like?ep={ep}"), None)
        .await
}

#[tauri::command]
pub async fn api_collect(state: State<'_, Api>, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    state
        .authed(method, &format!("/api/videos/{id}/collect"), None)
        .await
}

#[tauri::command]
pub async fn api_follow(state: State<'_, Api>, uid: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    state
        .authed(method, &format!("/api/users/{uid}/follow"), None)
        .await
}

/* ───────────────────────── 本地缓存 ───────────────────────── */

#[tauri::command]
pub async fn cache_clear() -> Result<(), String> {
    crate::media::clear_caches();
    Ok(())
}

/* ───────────────────────── 吃瓜帖子互动 ───────────────────────── */

#[tauri::command]
pub async fn api_post_like(state: State<'_, Api>, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    state
        .authed(method, &format!("/api/posts/{id}/like"), None)
        .await
}

#[tauri::command]
pub async fn api_post_collect(state: State<'_, Api>, id: i64, on: bool) -> Result<Value, String> {
    let method = if on { "POST" } else { "DELETE" };
    state
        .authed(method, &format!("/api/posts/{id}/collect"), None)
        .await
}

#[tauri::command]
pub async fn api_post_comments(
    state: State<'_, Api>,
    id: i64,
    page: u32,
    size: u32,
) -> Result<Value, String> {
    state
        .get(&format!("/api/posts/{id}/comments?page={page}&size={size}"))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playback() -> Value {
        json!({ "id": "12", "ep": 3, "manifest_method": "GET",
            "streams": { "main": "https://media.example.test/main.m3u8", "hevc": "" },
            "cdn": { "m3u8Enabled": true, "otherEnabled": false,
                "authKey": "x-test-auth", "authValue": "fixture-secret", "app": "fixture-app" } })
    }

    #[test]
    fn playback_contract_is_adapted_without_exposing_cdn_credentials() {
        let normalized = normalize_playback(playback(), 12, 3).unwrap();
        assert_eq!(normalized["ep_num"], 3);
        assert_eq!(
            normalized["video_url"],
            "https://media.example.test/main.m3u8"
        );
        assert!(normalized["proxy_url"]
            .as_str()
            .unwrap()
            .contains("/hls?u="));
        assert!(!normalized.to_string().contains("fixture-secret"));
    }

    #[test]
    fn wrong_identity_or_missing_stream_is_rejected() {
        assert!(normalize_playback(playback(), 13, 3).is_err());
        assert!(normalize_playback(playback(), 12, 4).is_err());
        let mut empty = playback();
        empty["streams"]["main"] = json!("");
        assert!(normalize_playback(empty, 12, 3).is_err());
    }
}
