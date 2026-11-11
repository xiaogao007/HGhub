<div align="center">

# HGhub

**黄果短剧第三方客户端 · Rust + Tauri 2**

Windows / Android 双端 · Fluent 设计 · 沉浸式竖屏播放
![alt text](docs/image.png)

</div>

> ⚠️ 本项目仅供 **学习与技术交流**，不提供、不存储、不分发任何音视频内容。
> 所有内容均来自第三方站点，请自行判断合规性，并在 24 小时内删除相关资源。

## 功能

- **首页**：沉浸式竖屏信息流，上下滑动切换剧集，双击/按钮点赞
- **统一播放页**：标题、作者与关注、可展开简介、话题标签、评分；右侧竖列集成 点赞 / 评论 / 收藏 / 选集 / 分享
- **播放器**：HLS + AES-128 解密播放、倍速、音量、画中画、影院模式、全屏、上下集切换
- **选集**：右侧停靠面板，画面自动让位；播完自动下一集
- **分类**：AI 短剧 / 漫剧 / 换脸 / 魔改 / 黄果吃瓜，支持 最新 / 最热 / 高分 排序与无限滚动
- **排行榜**：热播榜 / 推荐榜 / 潜力榜
- **搜索**：关键词搜索、搜索历史、热门题材、分页加载
- **吃瓜**：图文与图文+视频混排，视频直接播放，纯图文在应用内阅读
- **我的**：登录/注册、收藏与观看历史、深浅主题、关于、清理缓存
- **双端外壳**：Windows 无边框标题栏 + 可折叠侧栏；Android 底部标签栏 + 安全区适配

## 技术栈

| 层 | 选型 |
| --- | --- |
| 壳 | Tauri 2（Rust 1.9x） |
| 前端 | React 19 + TypeScript + Vite 7 + Tailwind CSS 4 |
| 播放 | hls.js（HLS / AES-128） |
| 网络 | reqwest + rustls |
| 图标 | 内联 SVG |

## 目录

```
.
├─ src/                      前端
│  ├─ api.ts                 接口封装 + 本地媒体协议 URL
│  ├─ store.tsx              全局状态（导航栈 / 用户 / 本地收藏 / 主题）
│  ├─ html.ts                吃瓜正文清洗与图片代理
│  ├─ components/            Feed（播放页）/ VideoSurface / PlayerControls / Panels / UI
│  └─ pages/                 Home / Search / Category / Profile / Posts
├─ src-tauri/                Rust 后端
│  └─ src/
│     ├─ net.rs              请求层（边缘信封 + 重试）
│     ├─ api.rs              业务命令
│     ├─ media.rs            封面解密 + HLS 代理 + 缓存
│     └─ session.rs          token 持久化
└─ docs/API.md               接口文档
```

## 开发 / 构建

```bash
pnpm install          # 首次安装
pnpm app:dev          # 桌面端热重载
pnpm app:build        # 打包 Windows 安装程序（NSIS）
```

Android（需 Android SDK + NDK，并配置 `ANDROID_HOME` / `NDK_HOME`）：

```bash
pnpm android:init     # 首次生成 android 工程
pnpm android:dev      # 真机 / 模拟器调试
pnpm android:build    # 产出 APK
```

### 环境注意

- 工程若放在 **exFAT** 盘（不支持符号链接），需保留 `.npmrc` 中的 `node-linker=hoisted`。
- `esbuild` 需在 `package.json` 的 `pnpm.onlyBuiltDependencies` 中放行，否则 `vite build` 缺少二进制。

## 实现要点

1. **边缘请求**：列表等请求使用 `POST /api/edge/request`；播放改用业务路径 `POST /api/videos/{id}/episodes/{ep}/playback`，经独立的 `POST /api/playback/request` 加密通道传输（ECDH P-256 → HKDF-SHA256 → AES-256-GCM）。会话过期会重新握手，密钥仅驻留内存；继续使用服务端签发的 Bearer 登录态。
2. **封面解密**：封面为 AES-128-CBC 密文，Rust 侧解密后经自定义协议输出，前端 `<img>` 直接使用。
3. **HLS 代理**：m3u8、嵌套清单、key 与分片经自定义协议转发。CDN 临时请求头按播放上下文留在 Rust 内存中，仅发送到对应媒体源；清单缓存按上下文隔离。
4. **自定义协议**：Windows 为 `http://hg.localhost/*`，Android 为 `https://hg.localhost/*`（由 Rust 侧按平台生成，避免 WebView 无法解析）。
5. **不做签名逆向**：只复用浏览器同源行为，接口字段均有文档与实测样本。

接口细节见 [docs/API.md](docs/API.md)。

## 免责声明

- 本项目与内容提供方无任何关联，作者不参与内容的制作、上传与分发。
- 使用者应自行遵守所在地法律法规；因使用本项目产生的任何后果由使用者自行承担。
- 若相关权利方认为本项目存在侵权，请通过 Issue 联系，我们会及时处理。

## License

[MIT](LICENSE)
## 常见问题

**1. `pnpm install` 报 `ERR_PNPM_EISDIR ... symlink`**

工程放在 exFAT / FAT32 盘时无法创建符号链接，保留 `.npmrc` 里的 `node-linker=hoisted` 即可。

**2. `tauri android build` 报 `Failed to create a symbolic link ... jniLibs`**

同样是 exFAT 不支持符号链接。可手动把 Rust 产物拷进 jniLibs，再直接跑 Gradle：

```bash
cp src-tauri/target/aarch64-linux-android/debug/libhuangguo_client_lib.so \
   src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a/

cd src-tauri/gen/android
./gradlew assembleDebug        # Windows: gradlew.bat assembleDebug
```

产物在 `src-tauri/gen/android/app/build/outputs/apk/`。

**3. Android 构建提示找不到 SDK / NDK**

需要设置 `ANDROID_HOME`（Android SDK 根目录）、`NDK_HOME`（`$ANDROID_HOME/ndk/<版本>`）与 `JAVA_HOME`（JDK 17）。

## LinuxDO
https://linux.do/
