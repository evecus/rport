# tunx

轻量级内网穿透工具（NAT traversal），支持 QUIC / TCP / WebSocket / XHTTP 多种传输模式，内置 Web 管理界面。

## 快速开始

```bash
# 不带参数直接运行：若当前目录没有 config.toml，会自动生成一份空模板
./tunx

# 或指定配置文件路径
./tunx -c /path/to/config.toml
```

首次启动时：

1. 若配置文件不存在，会自动生成一份空模板（`mode = "server"`，`[server]`/`[client]` 段留空）
2. 若 `[web].password_hash` 为空，会自动生成一个随机密码，写回配置文件，并**仅打印一次**在启动日志里：

   ```
   ══════════════════════════════════════════════════════════════
   首次启动，已生成 Web UI 登录密码（仅此一次打印，请妥善保存）:
     用户名: admin
     密　码: XXXXXXXXXXXXXXXXXXXX
   配置文件: config.toml
   ══════════════════════════════════════════════════════════════
   ```

3. 打开浏览器访问 `http://<服务器IP>:1080`，用上面的账号密码登录

登录后，如果配置还不完整（比如 server_addr / token 等必填项没填），Web UI 会提示"待配置"，在页面上填完并保存即可自动启动。之后修改配置、切换 server/client 模式，保存后都会**热更新生效**，不需要重启进程。

## Web UI 功能

- **概览**：服务端模式下查看已连接客户端列表（在线状态、心跳时间、注册的代理、实时流量）；客户端模式下查看已配置代理的连接数和流量统计
- **配置**：可视化编辑配置（模式切换、传输方式、TLS 模式、代理增删、Web 账号密码），保存后立即热更新
- 默认监听 `0.0.0.0:1080`，可在 `[web].listen` 修改

## 从源码构建

需要 Rust 1.75+、Node.js 18+、protoc。

```bash
# 前端会在 cargo build 时自动构建（依赖 build.rs 里的 npm ci && npm run build）
# 也可以手动先构建一遍，加快后续 cargo build：
cd web-ui && npm ci && npm run build && cd ..

cargo build --release
```

前端源码在 `web-ui/`，构建产物 `web-ui/dist` 会通过 `rust-embed` 内嵌进最终的 `tunx` 二进制，运行时不依赖外部文件。

## 配置文件说明

见 `config.toml` 示例文件，包含完整的字段注释。核心结构：

```toml
mode = "server"  # 或 "client"

[server]  # mode = "server" 时生效
...

[client]  # mode = "client" 时生效
...

[web]  # server/client 模式下都生效，Web 管理界面配置
enabled = true
listen = "0.0.0.0:1080"
username = "admin"
password_hash = ""  # 留空则首次启动自动生成
```

## 已知限制

- **热重启的代价**：修改 `bind_addr`、`transport`、TLS 等核心参数并保存后，服务端会重新绑定端口、断开所有客户端连接。客户端有自动重连机制，会在几秒内重新连上。
- **登录态不持久化**：Web UI 的登录 token 只保存在内存里，进程重启后需要重新登录（用户名密码不受影响）。
- **配置编辑器的传输模式**：Web UI 目前只支持四种单一传输模式（quic/tcp/websocket/xhttp）的可视化编辑；服务端支持的组合模式（如 `quic+tcp`）需要直接编辑配置文件后重新加载。
- **忘记密码**：手动编辑配置文件，把 `[web]` 下的 `password_hash` 清空，重启进程即可重新生成随机密码。

## 项目结构

```
crates/
  tunx-common/   配置、错误类型、流量统计（metrics）、流量计数包装器（counting_io）
  tunx-proto/    gRPC/protobuf 协议定义
  tunx/          主程序：server/client 核心逻辑 + web 管理界面
    src/
      server/    服务端：QUIC/TCP/WebSocket/XHTTP 监听、session 管理、代理转发
      client/    客户端：连接服务端、断线重连、代理转发
      web/       Web UI：axum 路由、鉴权、REST API、内嵌前端静态资源
      runtime.rs 运行时状态中枢：配置读写、热重启
web-ui/          Vue3 + Vite 前端源码
```
