# 黄果短剧 · 客户端 API 文档（v1）

> 面向 HGhub 客户端（Rust + Tauri 2 / Windows + Android）。
> 全部结论来自真实站点抓取与实测，未做签名逆向，只复用站点自身「浏览器同源请求」的行为。

## 0. 基础信息

| 项 | 值 |
| --- | --- |
| 稳定地址 | `https://huangguoai.com` |
| 备用地址（地址发布页） | `https://huangguoai.pages.dev/` |
| 图片 CDN | `https://pic.drleao.cn`（`new2.drleao.cn` / `new.drleao.cn` / `image.drleao.cn` 镜像） |
| 视频 CDN | `https://yd-hls.hzjvlo.cn`、`https://yd-lphu-hls.hzjvlo.cn`、`https://tp3.drleao.cn` |
| 默认 UA | `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36` |
| 必要请求头 | `User-Agent`、`Accept: application/json`、`Referer: https://huangguoai.com/` |

### 0.1 统一响应信封

所有 `/api/**` 接口都返回：

```json
{ "data": <任意>, "status": 1, "msg": "", "req_time": 1793707998 }
```

- `status === 1` 成功；`status === 0` 业务失败，`msg` 为中文提示。
- 401/403 表示未登录或会话失效，客户端应清空 token 并跳登录。

### 0.2 边缘代理信封（关键）

这是原有的桥接入口，由边缘层转发业务请求。客户端的列表等调用目前仍使用此入口，登录态用 Bearer。
播放接口已同时升级业务路径、加密传输和 CDN 临时凭据，见 §6.2，不能继续复用旧 GET 取址。

```
POST https://huangguoai.com/api/edge/request
Content-Type: application/json
Authorization: Bearer <token>        // 仅需登录的接口才带
```

```json
{
  "method": "GET",                        // 原方法：GET/POST/DELETE
  "path": "/api/videos?page=1&size=20",   // 原路径 + 查询串
  "body": "",                             // 原请求体（Base64），GET 传空串
  "content_type": ""                      // 原请求体类型，GET 传空串
}
```

- `body` 是**原始字节的 Base64**（不是 JSON 再包一层）。POST JSON 时：
  `body = base64(utf8(JSON字符串))`，`content_type = "application/json"`。
- 响应 = 真实接口信封，原样返回。

登录示例（`body` 解码为 `{"username":"<你的账号>","password":"<你的密码>"}`）：

```http
POST /api/edge/request
{"method":"POST","path":"/api/account/loginByPassword",
 "body":"eyJ1c2VybmFtZSI6Imh1YW5nZ3VvaHViIiwicGFzc3dvcmQiOiIxMjM0NTZodWIifQ==",
 "content_type":"application/json"}
```

### 0.3 直连（可选）

公开 GET 可绕过信封直连，例如 `GET /api/videos/10769/comments?ep=1&page=1&size=4` 实测 200。
但**带登录态的接口直连会 401**（`/api/account/me`、点赞、收藏、发评论），必须走信封 + Bearer。
本项目为统一行为，**全部走信封**。

---

## 1. 登录 / 账号

### 1.1 账号密码登录

```
POST /api/account/loginByPassword
body: {"username":"<账号>","password":"<密码>"}
```

```json
{
  "token": "b860ef9f...",
  "user": {
    "id": 302015, "username": "demo", "nickname": "demo",
    "avatar": "", "avatar_color": "#9B5FC7", "sign": "", "desc": "",
    "user_type": "normal", "channel_code": "",
    "counts": { "collect": 0, "like": 0 }
  }
}
```

- 服务端**不下发 Set-Cookie**（实测响应头无 `Set-Cookie`），登录态完全靠返回的 `token`。
- token 有效期约 7 天，客户端需持久化。
- 密码 6–20 位；注册需「字母+数字组合」。

### 1.2 注册

```
POST /api/account/registerByPassword
body: {"username":"...","password":"...","confirm_password":"...","email":"可选"}
```

账号 4–20 位字母/数字/下划线；响应同 1.1。

### 1.3 当前用户

```
GET /api/account/me        (Bearer)
```

```json
{
  "id": 302015, "nickname": "demo", "username": "demo",
  "avatar": "", "avatar_color": "#9B5FC7",
  "counts": { "like": 0, "collect": 0, "following": 0, "comment": 0, "history": 0 },
  "interactions": {
    "like_video_ids": [], "favorite_video_ids": [],
    "like_post_ids": [], "favorite_post_ids": [],
    "follow_user_ids": [], "like_video_episodes": []
  }
}
```

`interactions.like_video_episodes` 形如 `[{"video_id":10769,"ep":1}]`，**点赞按「影片+集」维度**。

### 1.4 退出

```
POST /api/account/logout   (Bearer)
```

---

## 2. 分类 / 列表

### 2.1 分类频道（固定 5 个）

| 频道 | 页面地址 | 分类接口 cate |
| --- | --- | --- |
| AI 成人短剧 | `/ai-duanju/` | `ai-duanju` |
| AI 成人漫剧 | `/ai-manju/` | `ai-manju` |
| AI 换脸 | `/ai-huanlian/` | `ai-huanlian` |
| AI 魔改 | `/ai-mogai/` | `ai-mogai` |
| 黄果吃瓜 | `/chigua/` | 走 `/api/posts`（见 §5） |

频道页另有 4 个标签：`latest`（最新更新，默认）、`hot`（当前热播）、`original`（独家原创）、`random`（随机推荐），
HTML 对应 `/ai-duanju/hot/`、`/ai-duanju/original/`、`/ai-duanju/random/`。
> 实测 JSON 列表接口不接受 `tab/sort`（返回值不变），需要完全一致时走 HTML 解析；一般场景用 JSON + 客户端排序。

### 2.2 首页 / 全站推荐流

```
GET /api/videos?page=1&size=20
```

### 2.3 分类列表

```
GET /api/videos/category/{cate}?page=1&size=20
```

### 2.4 列表响应

```json
{ "items": [ { "...VideoCard" } ], "pagination": { "page": 1, "pages": 1069, "size": 20, "total": 3206 } }
```

`VideoCard` 字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `id` | int | 剧集 ID（`/video/{id}/`） |
| `title` | string | 标题 |
| `cover` | string | 封面 URL（**AES 加密**，见 §7） |
| `description` | string | 简介（较长，可能含换行） |
| `tags` | string[] | 题材标签 |
| `score` | number | 评分（0–10） |
| `episode_count` | int | 已更新集数 |
| `total_episodes` | int | 总集数（可能为 0） |
| `duration` | string | 单集时长（秒，字符串） |
| `hot` | int | 热度值 |
| `like_count` / `collect_count` / `comment_count` | int | 互动数 |
| `is_finished` | bool | 是否完结 |
| `is_original` | int | 1 = 黄果原创 |
| `created_at` | string | `2026-08-07 19:16:47` |
| `preview_url` | string | 预览片（可能为空） |
| `author` | object | `{id,name,avatar}` |

---

## 3. 排行榜

| 榜单 | 页面 | 接口 |
| --- | --- | --- |
| 热播榜 | `/ranks/hot/` | `GET /api/ranks/hot` |
| 推荐榜 | `/ranks/recommend/` | `GET /api/ranks/recommend` |
| 潜力榜 | `/ranks/potential/` | `GET /api/ranks/potential` |

```json
{
  "type": "hot",
  "items": [{
    "rank": 1, "video_id": 2833, "title": "女友帮我操闺蜜",
    "cover": "https://pic.drleao.cn/...", "tags": ["现代","大男主","都市","绿帽"],
    "score": 9.5, "cate_name": "AI成人短剧", "episode_count": 12, "is_finished": false,
    "metric_label": "热力指数", "metric_value": 6063,
    "metrics": { "collect": 11892, "comment": 341, "like": 27285, "play": 30163724 }
  }]
}
```

> 榜单固定返回 **20 条**，忽略 `page/size`；三个榜单 `metric_label` 不同。

---

## 4. 搜索

```
GET /api/search?q=<关键词>&page=1&size=20
```

> 参数名是 **`q`**（传 `keyword` 会返回 `status:0, msg:"请输入搜索关键词"`）。

```json
{
  "engine": "vector", "keyword": "女王", "type": "video",
  "items": [ { "...VideoCard" } ],
  "pagination": { "page": 1, "pages": 2, "size": 20, "total": 40 }
}
```

HTML 版：`/search/?keyword=xxx`；分页 `/search/video/{关键词}/{页码}/`，帖子分栏 `/search/post/{关键词}/`。

---

## 5. 吃瓜（帖子）

```
GET /api/posts?page=1&size=20     // 列表
GET /api/posts/{id}               // 详情（含 video_url）
```

列表项：

```json
{ "id": 65, "title": "...", "desc": "", "cover": "https://pic.drleao.cn/...",
  "tags": ["抖音网红","黑料","吃瓜"], "hot": 52728,
  "created_at": "2026-08-13 17:08:00", "publish_time": "2026-08-14 01:07:59" }
```

详情 `data` 字段：`author, content, counts, cover, created_at, desc, id, interaction, publish_time, tag_links, tags, title, video_api, video_iv, video_key, video_url`。

---

## 6. 短剧详情 / 播放

### 6.1 详情（一次拿全）

```
GET /api/videos/{id}
```

| 字段 | 说明 |
| --- | --- |
| `id` / `title` / `description` | 基本信息 |
| `cover` | 封面（加密，§7） |
| `tags` / `tag_links` | `["现代","熟女"]` / `[{name,url}]` |
| `score` | 评分 |
| `episode_count` | 已更新集数 |
| `episodes` | `[{ep_num,title,duration,status}]`，完整集列表 |
| `counts` | `{collect,comment,hot,like}` |
| `author` | `{id,name,followers,following,is_following,like_count}` |
| `interaction` | `{collected,liked,liked_ep}`（登录后才有真实值） |
| `video_url` | 第 1 集 m3u8（SSR 预置） |
| `video_url_hevc` | H.265 版本（可能为空） |
| `is_finished` / `is_original` / `video_type` | 状态位 |

### 6.2 取指定集播放地址

**2026-11 兼容说明：** 当前取址业务路径为 `POST /api/videos/{id}/episodes/{ep}/playback`，JSON 请求体 `{}`，通过 `POST /api/playback/request` 加密通道发送。
旧 GET `/api/videos/{id}/play`（包括放入加密通道的旧路径）可能仍返回 `status: 1`，但 `video_url` 为空，不能视为可播放。
新响应 `data` 包含 `id`、`ep`、`manifest_method: "GET"`、`streams: { main, hevc }`、`cdn` 与 `timestamp`。
`cdn` 的 `m3u8Enabled` / `otherEnabled` 决定是否添加 `authKey: authValue` 和 `x-cdn-app: app`。
这些凭据只保存在 Rust 内存中；前端获得 `proxy_url`，本地代理通过 `c` 上下文编号为对应源的请求附加凭据。
嵌套 m3u8 继续走 `/hls`，key 与媒体分片走 `/seg`，上下文贯穿整个请求链。
客户端在 `src-tauri/src/edge_crypto.rs` 实现当前公开的 `dx150-edge-v1` 协议：

- `hello`：发送临时 P-256 公钥，使用服务端公钥协商共享密钥。
- HKDF-SHA256：结合服务端 salt，以及协议/通道/site/kid/sid 上下文，分别派生请求和响应密钥。
- `call`：AES-256-GCM 加密原始业务信封；nonce 使用 12 字节、大端序号，AAD 绑定方向、序号和服务端校正时间。
- 校验响应的会话、序号、时间和认证标签，解密内层 HTTP 响应，再解析原有 JSON 信封。
- 会话仅驻留内存；超过有效期或 16384 次请求后换钥。服务端明确返回 `session_expired` 时最多重握手重试一次。

本地回归测试（不请求站点或媒体；需 Node.js）：`cd src-tauri && cargo test --offline`。
线上握手自检（只请求无效 ID，不取媒体）：`cargo test live_playback_handshake -- --ignored`。
线上取址结构自检（只检查地址非空与字段，不下载媒体）：`cargo test live_playback_contract -- --ignored`。
原有依赖实时媒体或短期签名 URL 的测试默认忽略，需显式 `--ignored` 才执行。

```
POST /api/videos/{id}/episodes/{集号}/playback
Content-Type: application/json
{}
```

```json
{ "id": 10769, "ep": 3, "manifest_method": "GET",
  "streams": { "main": "https://media.example.test/index.m3u8?...", "hevc": "" },
  "cdn": { "m3u8Enabled": true, "otherEnabled": false,
    "authKey": "<header-name>", "authValue": "<temporary-value>", "app": "<app>" },
  "timestamp": 0 }
```

- 主清单从 `streams.main` 获取。地址和 CDN 凭据具有时效，**播放前现取**，不要长期缓存；前端经规范化结果的 `proxy_url` 播放。

### 6.3 播放实现

m3u8 内部为 AES-128 加密，播放列表自带 key URI：

```
#EXT-X-KEY:METHOD=AES-128,URI="https://tp3.drleao.cn/.../crypt.key?auth_key=...",IV=0x...
```

- 桌面 / Android WebView 统一用 **hls.js**（`<video>` + `Hls.loadSource`）。
- key 与分片实测无需 Referer 即可 200；为避免 WebView 的 CORS 限制，客户端把 m3u8/分片统一经本地协议代理（§8.3）。

---

## 7. 图片（封面）解密

封面 CDN 返回的**不是图片**，而是 AES-CBC 密文（`Content-Type: binary/octet-stream`）。

```
算法: AES-128-CBC / NoPadding
Key : f5d965df75336270   (ASCII, 16B)
IV  : 97b60394abc2fbe1   (ASCII, 16B)
```

解密后：

1. 按 PKCS#7 去尾填充（末字节 = 填充长度，校验后截断）。
2. 校验魔数：JPEG `FFD8` / PNG `89504E47` / GIF `47494638` / WEBP `52494646`。
3. JPEG 可截到最后一个 `FFD9`。

> 未加密的老图（直接是 `FFD8...`）先探测魔数，命中就直接返回，不要解密。

---

## 8. 客户端落地（本项目实现）

### 8.1 请求层

```
reqwest + rustls，统一 UA/Referer/Accept，
所有业务请求 → POST /api/edge/request（信封），自动附加 Bearer；
超时 20s，失败重试 3 次（站点偶发 TLS 抖动，实测必要）。
```

### 8.2 能力映射

| 客户端能力 | 上游接口 |
| --- | --- |
| 首页推荐流 | `GET /api/videos` |
| 分类列表 | `GET /api/videos/category/{cate}` |
| 排行榜 | `GET /api/ranks/{hot|recommend|potential}` |
| 搜索 | `GET /api/search?q=` |
| 吃瓜 | `GET /api/posts` |
| 详情 + 集数 | `GET /api/videos/{id}` |
| 播放地址 | `GET /api/videos/{id}/play?ep=` |
| 评论列表 | `GET /api/videos/{id}/comments?ep=&page=&size=` |
| 发评论 | `POST /api/videos/{id}/comments?ep=` body `{text}`（回复带 `reply_id`） |
| 评论回复 | `GET /api/videos/{id}/comments/{cid}/replies?page=&size=` |
| 点赞（按集） | `POST|DELETE /api/videos/{id}/like?ep=` |
| 收藏 | `POST|DELETE /api/videos/{id}/collect` |
| 关注作者 | `POST|DELETE /api/users/{uid}/follow` |
| 登录 / 我的 | §1 |

### 8.3 本地协议

| 协议 | 用途 |
| --- | --- |
| `hg://localhost/img?u=<urlencode(封面URL)>` | 取图 → AES 解密 → 返回 `image/jpeg`，带内存 LRU |
| `hg://localhost/hls?u=<urlencode(m3u8)>` | 取播放列表，把 key/分片 URI 改写为本地协议后返回 |
| `hg://localhost/seg?u=<urlencode(ts/key)>` | 代理分片与 key 字节 |

桌面实际为 `http://hg.localhost/...`，Android 为 `https://hg.localhost/...`；WebView 内不存在跨域问题。

### 8.4 评论响应

```json
{
  "ep": 1, "revision": ":v10", "reset": false,
  "items": [{
    "id": 131322, "user_id": 251137, "nickname": "DADADA2027",
    "avatar": "", "avatar_color": "#D0567F",
    "text": "...", "content": "...",
    "time": "1天前", "created_at": "2026-11-02 09:19:43",
    "like_count": 2, "liked": false, "reply_count": 0, "reply_id": 0,
    "reply_list": [], "is_pinned": false, "ip_region": ""
  }],
  "pagination": { "page": 1, "pages": 1, "size": 4, "total": 3 }
}
```

### 8.5 踩坑清单

1. 评论首页建议 `size=4`（与站点一致），翻页 `size=12`；列表 `size=20`。
2. 榜单忽略分页，固定 20 条。
3. 搜索参数是 `q`。
4. 分类 tab 参数对 JSON 无效。
5. `auth_key` 会过期：播放前现取 `play`；图片 URL 失效时回落到列表里的新 URL。
6. 站点偶发 TLS 握手失败（本地 DNS 假 IP / 代理导致），客户端必须重试。
7. 未登录时 `interaction` 恒为 `{collected:false,liked:false}`，不要当真实状态渲染。
8. 点赞、收藏、评论的 `ep` 必须放 **query**，放 body 不生效。
---

## 附：黄果吃瓜（帖子）接口

吃瓜内容不是剧集，走 `/api/posts` 体系，详情里常同时带**视频 + 图文**。

| 用途 | 方法与路径 | 说明 |
| --- | --- | --- |
| 列表 | `GET /api/posts?page=1&size=20` | 返回 `{items,pagination}`，字段 `id/title/cover/tags/hot/publish_time` |
| 详情 | `GET /api/posts/{id}` | 见下方字段表 |
| 点赞 | `POST /api/posts/{id}/like` / `DELETE` | 需登录 |
| 收藏 | `POST /api/posts/{id}/collect` / `DELETE` | 需登录 |
| 评论列表 | `GET /api/posts/{id}/comments?page=1&size=20` | 返回 `{items,pagination}` |

详情 `data` 主要字段：

```jsonc
{
  "id": 65,
  "title": "……",
  "cover": "https://pic.drleao.cn/...jpeg?auth_key=...",
  "content": "<p>正文 HTML，内含 <img> 图集</p>",   // 需要清洗后渲染，图片走本地代理
  "video_url": "https://xxx/videos5/xxx/xxx.m3u8?auth_key=...",  // 部分帖子没有该字段
  "author": { "id": 156309, "name": "黄果吃瓜侠", "avatar": "...", "is_following": false },
  "counts": { "like": 240, "comment": 0, "collect": 110, "share": 0, "hot": 53348 },
  "interaction": { "liked": false, "collected": false },
  "tags": ["吃瓜", "..."],
  "publish_time": "2026-08-14 01:07:59"
}
```

前端处理约定：

- `video_url` 存在 → 直接进沉浸式播放页播放；
- `content` 非空 → 右侧栏常驻「图文」入口，正文经 `DOMParser` 清洗后渲染，`<img src>` 改写为 `hg://localhost/img?u=` 走本地代理（避免防盗链 403）。
