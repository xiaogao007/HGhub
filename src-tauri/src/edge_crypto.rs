//! 上游公开的 dx150-edge-v1 传输协议。会话与密钥仅保存在内存中。

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use once_cell::sync::Lazy;
use ring::{aead, agreement, hkdf, rand::SystemRandom};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const MAX_SEQUENCE: u32 = 16384;
pub static PLAYBACK: Lazy<EncryptedChannel> = Lazy::new(|| EncryptedChannel::new("playback"));

struct Session {
    kid: String,
    sid: String,
    info: String,
    clock_offset: i64,
    expires: Instant,
    request_key: aead::LessSafeKey,
    response_key: aead::LessSafeKey,
}

pub struct EncryptedChannel {
    scope: &'static str,
    // 只在握手与分配序号时加锁，业务请求可并发。
    session: Mutex<Option<(Arc<Session>, u32)>>,
}

#[derive(Deserialize)]
struct Hello {
    v: u8,
    #[serde(rename = "type")]
    kind: String,
    kid: String,
    sid: String,
    site: String,
    server_time: i64,
    expires_at: i64,
    server_pub: String,
    salt: String,
}

fn now() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|_| "系统时间无效".into())
}

fn valid_id(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn validate_hello(hello: &Hello) -> Result<u64, String> {
    let lifetime = hello.expires_at.checked_sub(hello.server_time).unwrap_or(0);
    if hello.v != 1
        || hello.kind != "hello"
        || !valid_id(&hello.kid, 1, 64)
        || !valid_id(&hello.sid, 16, 128)
        || !(0..=9_007_199_254_740_991).contains(&hello.server_time)
        || !(0..=9_007_199_254_740_991).contains(&hello.expires_at)
        || !(1..=256).contains(&hello.site.len())
        || !hello
            .site
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".:[]-".contains(&b))
        || !(1..=600).contains(&lifetime)
    {
        return Err("播放接口握手响应无效".into());
    }
    Ok(lifetime as u64)
}

fn derive_key(
    shared: &[u8],
    salt: &[u8],
    info: &str,
    direction: &str,
) -> Result<aead::LessSafeKey, String> {
    let material = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(shared);
    let context = format!("{info}\n{direction}");
    let parts = [context.as_bytes()];
    let output = material
        .expand(&parts, hkdf::HKDF_SHA256)
        .map_err(|_| "播放密钥派生失败")?;
    let mut bytes = [0u8; 32];
    output.fill(&mut bytes).map_err(|_| "播放密钥派生失败")?;
    let key = aead::UnboundKey::new(&aead::AES_256_GCM, &bytes).map_err(|_| "播放密钥无效")?;
    Ok(aead::LessSafeKey::new(key))
}

fn nonce(sequence: u32) -> aead::Nonce {
    let mut bytes = [0u8; 12];
    bytes[8..].copy_from_slice(&sequence.to_be_bytes());
    aead::Nonce::assume_unique_for_key(bytes)
}

impl Session {
    fn aad(&self, direction: &str, sequence: u32, timestamp: i64) -> aead::Aad<Vec<u8>> {
        aead::Aad::from(format!("{}\n{direction}\n{sequence}\n{timestamp}", self.info).into_bytes())
    }

    fn encrypt(
        &self,
        request: &Value,
        token: Option<&str>,
        sequence: u32,
        timestamp: i64,
    ) -> Result<Value, String> {
        let mut packet = json!({ "request": request });
        if let Some(token) = token.filter(|t| !t.is_empty()) {
            packet["authorization"] = json!(format!("Bearer {token}"));
        }
        let mut bytes = serde_json::to_vec(&packet).map_err(|_| "播放请求编码失败")?;
        self.request_key
            .seal_in_place_append_tag(
                nonce(sequence),
                self.aad("req", sequence, timestamp),
                &mut bytes,
            )
            .map_err(|_| "播放请求加密失败")?;
        Ok(
            json!({ "v": 1, "type": "call", "kid": self.kid, "sid": self.sid,
            "seq": sequence, "ts": timestamp, "ciphertext": URL_SAFE_NO_PAD.encode(bytes) }),
        )
    }

    fn decrypt(&self, result: &Value, sequence: u32, timestamp: i64) -> Result<Value, String> {
        if result["v"] != 1
            || result["type"] != "result"
            || result["kid"] != self.kid
            || result["sid"] != self.sid
            || result["seq"] != sequence
            || result["ts"] != timestamp
        {
            return Err("播放接口响应与请求不匹配".into());
        }
        let mut bytes = URL_SAFE_NO_PAD
            .decode(result["ciphertext"].as_str().unwrap_or(""))
            .map_err(|_| "播放响应编码无效")?;
        let plaintext = self
            .response_key
            .open_in_place(
                nonce(sequence),
                self.aad("res", sequence, timestamp),
                &mut bytes,
            )
            .map_err(|_| "播放响应校验失败")?;
        let inner: Value = serde_json::from_slice(plaintext).map_err(|_| "播放响应格式无效")?;
        let status = inner["status"]
            .as_u64()
            .filter(|s| (200..=599).contains(s))
            .ok_or("播放响应状态无效")?;
        let headers = inner["headers"].as_object().ok_or("播放响应头无效")?;
        if !headers.values().all(|values| {
            values
                .as_array()
                .is_some_and(|v| v.iter().all(Value::is_string))
        }) {
            return Err("播放响应头无效".into());
        }
        if status == 401 || status == 403 {
            return Err("401".into());
        }
        if !(200..300).contains(&status) {
            return Err(format!("播放接口 HTTP {status}"));
        }
        let body = STANDARD
            .decode(inner["body"].as_str().ok_or("播放响应正文缺失")?)
            .map_err(|_| "播放响应正文编码无效")?;
        serde_json::from_slice(&body).map_err(|_| "播放响应不是有效 JSON".into())
    }
}

impl EncryptedChannel {
    fn new(scope: &'static str) -> Self {
        Self {
            scope,
            session: Mutex::new(None),
        }
    }

    async fn reserve(
        &self,
        client: &reqwest::Client,
        base: &str,
    ) -> Result<(Arc<Session>, u32), String> {
        let mut cached = self.session.lock().await;
        if let Some((session, sequence)) = cached.as_mut() {
            if session.expires > Instant::now() + Duration::from_secs(5) && *sequence < MAX_SEQUENCE
            {
                *sequence += 1;
                return Ok((session.clone(), *sequence));
            }
        }
        let private =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &SystemRandom::new())
                .map_err(|_| "播放握手密钥生成失败")?;
        let public = private
            .compute_public_key()
            .map_err(|_| "播放握手公钥生成失败")?;
        let response = client.post(format!("{base}/api/{}/request", self.scope))
            .header("Referer", format!("{base}/"))
            .timeout(Duration::from_secs(8))
            .json(&json!({ "v": 1, "type": "hello", "client_pub": URL_SAFE_NO_PAD.encode(public.as_ref()) }))
            .send().await.map_err(|_| "播放接口握手失败，请检查网络")?;
        if !response.status().is_success() {
            return Err(format!("播放握手 HTTP {}", response.status()));
        }
        let hello: Hello = response.json().await.map_err(|_| "播放握手响应格式无效")?;
        let lifetime = validate_hello(&hello)?;
        let server_public = URL_SAFE_NO_PAD
            .decode(&hello.server_pub)
            .map_err(|_| "播放握手公钥无效")?;
        let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, server_public);
        let shared = agreement::agree_ephemeral(private, &peer, |key| key.to_vec())
            .map_err(|_| "播放握手密钥协商失败")?;
        let salt = URL_SAFE_NO_PAD
            .decode(&hello.salt)
            .map_err(|_| "播放握手盐值无效")?;
        if salt.len() != 32 {
            return Err("播放握手盐值无效".into());
        }
        let info = format!(
            "dx150-edge-v1\n{}\n{}\n{}\n{}",
            self.scope, hello.site, hello.kid, hello.sid
        );
        let session = Arc::new(Session {
            request_key: derive_key(&shared, &salt, &info, "req")?,
            response_key: derive_key(&shared, &salt, &info, "res")?,
            kid: hello.kid,
            sid: hello.sid,
            info,
            clock_offset: hello.server_time - now()?,
            expires: Instant::now() + Duration::from_secs(lifetime),
        });
        *cached = Some((session.clone(), 1));
        Ok((session, 1))
    }

    pub async fn call(
        &self,
        client: &reqwest::Client,
        base: &str,
        request: &Value,
        token: Option<&str>,
    ) -> Result<Value, String> {
        // 只对服务器明确拒绝的过期会话重握手一次；不重放结果未知的请求。
        for attempt in 0..2 {
            let (session, sequence) = self.reserve(client, base).await?;
            let timestamp = now()? + session.clock_offset;
            let packet = session.encrypt(request, token, sequence, timestamp)?;
            let response = client
                .post(format!("{base}/api/{}/request", self.scope))
                .header("Referer", format!("{base}/"))
                .json(&packet)
                .send()
                .await
                .map_err(|_| "播放接口请求失败，请重试")?;
            let status = response.status();
            let result: Value = response.json().await.map_err(|_| "播放接口返回格式无效")?;
            if !status.is_success() {
                if result["error"] == "session_expired" {
                    let mut cached = self.session.lock().await;
                    if cached
                        .as_ref()
                        .is_some_and(|(current, _)| Arc::ptr_eq(current, &session))
                    {
                        *cached = None;
                    }
                    if attempt == 0 {
                        continue;
                    }
                }
                return Err(format!("播放接口 HTTP {status}"));
            }
            return session.decrypt(&result, sequence, timestamp);
        }
        Err("播放会话已到期，请重试".into())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
    };

    pub(crate) struct MockServer(Child, pub(crate) String);
    impl MockServer {
        pub(crate) fn start() -> Self {
            let mut child = Command::new("node")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../tests/fixtures/edge-server.mjs"
                ))
                .stdout(Stdio::piped())
                .spawn()
                .expect("start local protocol fixture");
            let mut address = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            assert!(address.starts_with("http://127.0.0.1:"));
            Self(child, address.trim().to_string())
        }
        fn client(&self) -> reqwest::Client {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap()
        }
    }
    impl Drop for MockServer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn request(scenario: &str) -> Value {
        json!({ "method": "GET", "path": format!("/api/videos/1/play?ep=1&scenario={scenario}"),
            "body": "", "content_type": "" })
    }

    #[tokio::test]
    async fn node_protocol_roundtrip_reuses_session_and_supports_concurrent_calls() {
        let server = MockServer::start();
        let client = server.client();
        let channel = EncryptedChannel::new("playback");
        let request = request("ok");
        let first = channel
            .call(&client, &server.1, &request, Some("fixture-token"))
            .await
            .unwrap();
        assert_eq!(
            first["data"]["video_url"],
            "https://media.example.test/test.m3u8"
        );
        assert_eq!(first["data"]["diagnostics"]["tokenSeen"], true);
        let (second, third) = tokio::join!(
            channel.call(&client, &server.1, &request, None),
            channel.call(&client, &server.1, &request, None)
        );
        let second = second.unwrap();
        let third = third.unwrap();
        assert_eq!(second["data"]["diagnostics"]["helloCount"], 1);
        assert_eq!(third["data"]["diagnostics"]["helloCount"], 1);
        assert_ne!(
            second["data"]["diagnostics"]["sequence"],
            third["data"]["diagnostics"]["sequence"]
        );
    }

    #[tokio::test]
    async fn expired_session_is_reestablished_once() {
        let server = MockServer::start();
        let channel = EncryptedChannel::new("playback");
        let result = channel
            .call(&server.client(), &server.1, &request("expired"), None)
            .await
            .unwrap();
        assert_eq!(result["data"]["diagnostics"]["helloCount"], 2);
        assert_eq!(result["data"]["diagnostics"]["sequence"], 1);
    }

    #[tokio::test]
    async fn sequence_limit_rotates_keys_instead_of_reusing_a_nonce() {
        let server = MockServer::start();
        let client = server.client();
        let channel = EncryptedChannel::new("playback");
        channel
            .call(&client, &server.1, &request("ok"), None)
            .await
            .unwrap();
        channel.session.lock().await.as_mut().unwrap().1 = MAX_SEQUENCE;
        let result = channel
            .call(&client, &server.1, &request("ok"), None)
            .await
            .unwrap();
        assert_eq!(result["data"]["diagnostics"]["helloCount"], 2);
        assert_eq!(result["data"]["diagnostics"]["sequence"], 1);
    }

    #[tokio::test]
    async fn altered_or_mismatched_responses_are_rejected() {
        let server = MockServer::start();
        let client = server.client();
        let channel = EncryptedChannel::new("playback");
        for (scenario, expected) in [
            ("tamper", "播放响应校验失败"),
            ("mismatch", "播放接口响应与请求不匹配"),
            ("forbidden", "401"),
        ] {
            let error = channel
                .call(&client, &server.1, &request(scenario), None)
                .await
                .unwrap_err();
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn invalid_handshake_lifetime_and_identifiers_are_rejected() {
        let mut hello: Hello = serde_json::from_value(json!({ "v": 1, "type": "hello", "kid": "fixture",
            "sid": "abcdefghijklmnop", "site": "fixture.test", "server_time": 100, "expires_at": 220,
            "server_pub": "", "salt": "" })).unwrap();
        assert_eq!(validate_hello(&hello).unwrap(), 120);
        hello.expires_at = i64::MAX;
        assert!(validate_hello(&hello).is_err());
        hello.expires_at = 220;
        hello.site = "fixture.test\nother".into();
        assert!(validate_hello(&hello).is_err());
    }

    #[tokio::test]
    #[ignore = "requires upstream network; requests an invalid ID, never media"]
    async fn live_playback_handshake() {
        let result = EncryptedChannel::new("playback")
            .call(
                &crate::net::CLIENT,
                crate::net::BASE,
                &json!({ "method": "GET", "path": "/api/videos/0/play?ep=1&page_texts=0",
                    "body": "", "content_type": "" }),
                None,
            )
            .await;
        // ID 0 的拒绝来自已通过 GCM 校验的内层 HTTP 响应。
        // 外层通道错误带有 StatusCode 的 reason phrase，不会误判成这里的数字状态。
        assert_eq!(result.unwrap_err(), "播放接口 HTTP 400");
    }

    #[tokio::test]
    #[ignore = "requires upstream network; prints schema only, never requests media"]
    async fn live_playback_contract() {
        let result = EncryptedChannel::new("playback")
            .call(
                &crate::net::CLIENT,
                crate::net::BASE,
                &json!({ "method": "POST", "path": "/api/videos/12/episodes/1/playback",
                "body": "e30=", "content_type": "application/json" }),
                None,
            )
            .await
            .unwrap();
        let data = &result["data"];
        println!(
            "{}",
            json!({ "status": result["status"], "fields": data.as_object().map(|v| v.keys().collect::<Vec<_>>()),
            "manifest_method": data["manifest_method"], "has_main_stream": data["streams"]["main"].as_str().is_some_and(|s| !s.is_empty()),
            "cdn_fields": data["cdn"].as_object().map(|v| v.keys().collect::<Vec<_>>()),
            "cdn_m3u8_enabled": data["cdn"]["m3u8Enabled"], "cdn_other_enabled": data["cdn"]["otherEnabled"] })
        );
        assert_eq!(result["status"], 1);
        assert!(data["streams"]["main"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
    }
}
