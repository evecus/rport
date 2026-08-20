# 本地编译自查清单

这份代码的 Rust 部分**没有在任何环境里跑过 `cargo check`/`cargo build`**（沙盒里没有 Rust 工具链，也连不上 rustup 官方源），
全靠手工比对签名和类型写成。前端部分已经在沙盒里用真实的 `npm run build` 反复验证过，可信度高很多。

请按下面顺序验证：

## 1. 先构建前端（独立验证，风险最低）

```bash
cd web-ui
npm ci
npm run build
```

预期：`web-ui/dist/index.html` 等文件生成，无报错。这一步在我的沙盒里已经反复跑通过，正常情况下你这边也应该没问题。

## 2. cargo check 整个 workspace

```bash
cargo check --all 2>&1 | tee /tmp/check.log
```

大概率会报错的地方，按可能性从高到低排列：

1. **新依赖版本冲突**：`axum 0.6` / `tower-http 0.4` / `argon2 0.5` / `rust-embed 8` 是否真的和现有 `hyper 0.14` / `tower 0.4` / Rust 1.75 兼容。如果报版本冲突，先尝试把 `rust-embed` 降到 `6` 系（对 1.75 更友好），`axum`/`tower-http` 版本本身选的是刻意匹配 hyper 0.14 时代的组合，冲突概率较低。

2. **axum 0.6 的 extractor/中间件签名**：`web/auth.rs` 里的 `require_auth<B>` 中间件、`web/mod.rs` 里 `route_layer` + `fallback` 的组合写法，是我按记忆中 axum 0.6 的标准写法写的，如果版本细节对不上，报错信息会直接指出具体哪个 trait bound 不满足，比较好改。

3. **`rust-embed` 的 `#[folder = "../../web-ui/dist"]` 路径**：这个路径是相对 `crates/tunx/Cargo.toml` 算的，我确认过目录层级是对的，但请务必先跑第 1 步生成 `dist/`，否则这里会直接编译失败并给出清晰报错（不是隐晦的路径错误，`build.rs` 会先检查并尝试自动构建）。

4. **`server/control.rs` 的大改动**：`run()` 函数签名从返回 `Result<()>` 改成 `Result<ServerHandle>`，`run_quic`/`run_tcp` 从接收 `SocketAddr` 改成接收预先 bind 好的 `Endpoint`/`TcpListener`。这是改动量最大的地方，如果报错，大概率是某处遗漏了同步调用点（我已经交叉检查过 `server/mod.rs`、`server/websocket.rs`、`server/xhttp.rs` 三处调用，但请重点看这里）。

5. **`client/control.rs` 的 metrics 参数穿透**：`run()` → `run_session()` → `run_session_quic/tcp/websocket()` 都加了 `metrics: &MetricsRegistry` 参数，四个转发函数调用点（QUIC-TCP / QUIC-UDP / TCP-transport-TCP / TCP-transport-UDP）都加了 `metrics: Arc<ProxyMetrics>`。如果某处类型对不上，通常是 `.await` 位置或者 `Arc`/`&` 引用层级的问题。

6. **`CountingIo` 的 `wrap_public_side`/`wrap_local_side` 语义**：这两个我特意用命名构造函数替代了容易出错的 bool 参数，逻辑上服务端包裹"公网侧"、客户端包裹"本地服务侧"，如果流量统计数字全是 0 或者上下行搞反了，先检查这两处调用是否用错了方向。

## 3. cargo clippy（可选但建议）

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

CI 里这一步是强制的（`-D warnings`），新代码里我尽量避免了明显的 clippy 警告（比如 needless clone、unused import），但没有实际跑过，可能还有遗漏。

## 4. 功能性验证（编译通过之后）

```bash
# 清空测试，验证首次启动流程
rm -f config.toml
cargo run --bin tunx
```

预期：
- 生成 `config.toml`（`mode = "server"`，`[server]`/`[client]` 留空）
- 日志打印随机密码
- 访问 `http://127.0.0.1:1080` 能看到登录页
- 登录后提示"待配置"
- 在"配置"页填写 server 配置并保存，几秒内状态应变为"运行中"
- 用一个真实客户端连接测试端口转发是否正常工作
- 修改配置（比如换个 token）保存，验证客户端能自动重连

## 5. 已知的架构性风险点（编译通过也未必没问题）

- **热重启端口释放时机**：`runtime.rs::restart()` 里 abort 旧 listener 后 `sleep(200ms)` 再重新 bind，这是个经验值，如果本地测试发现偶尔报"端口已占用"，可以调大这个 sleep，或者改成重试 bind 几次。
- **`session.rs` 里 `#[allow(dead_code)]` 标记的 `session_id`/`client_id` 字段**：现在被 `web/api.rs` 真实读取了，这两个属性标记已经不准确但无害，可以顺手删掉。
- **metrics 内存不会无限增长，但有一个已知的迟滞清理**：server 端 session 断开后，对应的 metrics 条目要等最多 60 秒的后台清理任务才会删除，这是刻意设计（让 UI 能看到"刚断开"的最后一次流量快照），不是 bug。
