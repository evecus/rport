use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── 运行模式 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Server,
    Client,
}

// ─── 统一配置 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunxConfig {
    /// 运行模式：server 或 client
    pub mode: Mode,

    /// 服务端配置（mode = "server" 时必填）
    #[serde(default)]
    pub server: Option<ServerConfig>,

    /// 客户端配置（mode = "client" 时必填）
    #[serde(default)]
    pub client: Option<ClientConfig>,

    /// Web 管理界面配置（可选，缺省则使用默认值 + 首次启动生成随机密码）
    #[serde(default)]
    pub web: WebConfig,
}

// ─── Web 管理界面配置 ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    /// 是否启用 Web UI，默认启用
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Web UI 监听地址，默认 "0.0.0.0:1080"
    #[serde(default = "default_web_listen")]
    pub listen: String,

    /// 登录用户名，默认 "admin"
    #[serde(default = "default_web_username")]
    pub username: String,

    /// 登录密码的 argon2 哈希值。
    /// 为空时：首次启动会自动生成一个随机密码，哈希写回配置文件，
    /// 并把明文密码打印在启动日志里（仅此一次）。
    #[serde(default)]
    pub password_hash: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: default_web_listen(),
            username: default_web_username(),
            password_hash: String::new(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_web_listen() -> String {
    "0.0.0.0:1080".to_string()
}
fn default_web_username() -> String {
    "admin".to_string()
}

// ─── QUIC 调优配置 ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicConfig {
    /// 拥塞控制算法
    /// - "new_reno"：保守稳定，默认值
    /// - "bbr"：高带宽延迟积网络下吞吐更高（实验性，quinn 标注 experimental）
    #[serde(default = "default_congestion")]
    pub congestion: String,

    /// 初始 MTU（字节），quinn 会在此基础上做 MTU 探测自动上调
    /// 典型值：1200（保守）/ 1400（常见）/ 1452（PPPoE）
    #[serde(default = "default_initial_mtu")]
    pub initial_mtu: u16,

    /// 单条连接最大接收窗口（字节），影响高延迟链路的吞吐上限
    /// BDP = 带宽(bps) × RTT(s)，建议设为 2–4 倍 BDP
    /// 默认 8 MiB，足够 100 Mbps × 200ms 的链路
    #[serde(default = "default_recv_window")]
    pub recv_window: u32,
}

impl Default for QuicConfig {
    fn default() -> Self {
        Self {
            congestion: default_congestion(),
            initial_mtu: default_initial_mtu(),
            recv_window: default_recv_window(),
        }
    }
}

fn default_congestion() -> String {
    "new_reno".to_string()
}
fn default_initial_mtu() -> u16 {
    1200
}
fn default_recv_window() -> u32 {
    8 * 1024 * 1024 // 8 MiB
}

// ─── Transport 模式 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ServerTransport {
    /// 仅 QUIC（UDP），默认
    #[default]
    #[serde(rename = "quic")]
    Quic,
    /// 仅 TCP+TLS+HTTP/2
    #[serde(rename = "tcp")]
    Tcp,
    /// WebSocket 传输（CDN 友好）
    #[serde(rename = "websocket")]
    Websocket,
    /// XHTTP/HTTP/2 传输（CDN 友好）
    #[serde(rename = "xhttp")]
    Xhttp,
    /// QUIC + TCP 同时监听同一端口
    #[serde(rename = "quic+tcp")]
    QuicTcp,
    /// QUIC + WebSocket 同时监听同一端口
    #[serde(rename = "quic+websocket")]
    QuicWebsocket,
    /// QUIC + XHTTP 同时监听同一端口
    #[serde(rename = "quic+xhttp")]
    QuicXhttp,
}

impl ServerTransport {
    /// 是否包含 QUIC（UDP）listener
    pub fn has_quic(&self) -> bool {
        matches!(
            self,
            Self::Quic | Self::QuicTcp | Self::QuicWebsocket | Self::QuicXhttp
        )
    }
    /// 是否包含 TCP listener
    pub fn has_tcp(&self) -> bool {
        matches!(self, Self::Tcp | Self::QuicTcp)
    }
    /// 是否包含 WebSocket listener
    pub fn has_websocket(&self) -> bool {
        matches!(self, Self::Websocket | Self::QuicWebsocket)
    }
    /// 是否包含 XHTTP listener
    pub fn has_xhttp(&self) -> bool {
        matches!(self, Self::Xhttp | Self::QuicXhttp)
    }
    /// 是否需要 TLS（TCP/WebSocket/XHTTP 模式需要正规证书）
    pub fn needs_public_tls(&self) -> bool {
        self.has_tcp() || self.has_websocket() || self.has_xhttp()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClientTransport {
    /// QUIC（UDP），默认
    #[default]
    #[serde(rename = "quic")]
    Quic,
    /// TCP+TLS+HTTP/2
    #[serde(rename = "tcp")]
    Tcp,
    /// WebSocket 传输（CDN 友好）
    #[serde(rename = "websocket")]
    Websocket,
    /// XHTTP/HTTP/2 传输（CDN 友好）
    #[serde(rename = "xhttp")]
    Xhttp,
}

// ─── Server Config ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// QUIC/gRPC 监听地址，默认 "0.0.0.0:7000"
    /// transport=tcp 时也绑此地址的 TCP 端口
    /// transport=both 时 UDP+TCP 同时绑此端口
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// 传输模式：quic / tcp / both，默认 quic
    #[serde(default)]
    pub transport: ServerTransport,

    /// 允许客户端使用的端口范围（inclusive）
    #[serde(default = "default_port_range")]
    pub proxy_port_range: (u16, u16),

    /// 认证 token，为空则不校验
    #[serde(default)]
    pub token: String,

    /// TLS 配置
    pub tls: ServerTlsConfig,

    /// 心跳超时（秒），超过则断开客户端
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_secs: u64,

    /// 日志等级，如 "info"、"debug"、"warn"、"error"
    /// 也支持细粒度写法，如 "tunx=debug,quinn=warn"
    /// 可被环境变量 RUST_LOG 覆盖
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// QUIC 传输层调优
    /// 仅 transport=quic/quic+* 时生效；tcp/websocket/xhttp 模式下被忽略
    #[serde(default)]
    pub quic: QuicConfig,

    /// WebSocket 路径（仅 websocket/quic+websocket 模式生效）
    /// 客户端连接时使用的 HTTP 路径，默认 "/tunx-ws"
    #[serde(default = "default_ws_path")]
    pub ws_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode")]
pub enum ServerTlsConfig {
    /// 自动通过 ACME (Let's Encrypt) DNS-01 申请证书（Cloudflare）
    /// 不需要 80 端口，域名无需解析到本机也可申请
    #[serde(rename = "acme")]
    Acme {
        /// 域名，必填（需在 Cloudflare 托管）
        domain: String,
        /// ACME 账号邮箱（Let's Encrypt 过期提醒用）
        email: String,
        /// Cloudflare API Token
        /// 需要权限：Zone:DNS:Edit + Zone:Zone:Read
        cf_api_token: String,
        /// 证书缓存目录
        #[serde(default = "default_acme_cache")]
        cache_dir: PathBuf,
        /// 使用 staging 环境（测试用，不消耗配额）
        #[serde(default)]
        staging: bool,
    },
    /// 手动提供证书文件
    #[serde(rename = "manual")]
    Manual {
        cert_file: PathBuf,
        key_file: PathBuf,
    },
    /// 自动生成自签名证书
    /// sni：证书的域名（建议填大厂域名伪装流量特征，如 "www.bing.com"）
    /// 客户端需配置相同的 tls_sni，并设置 tls_skip_verify = true
    #[serde(rename = "self_signed")]
    SelfSigned {
        #[serde(default = "default_self_signed_sni")]
        sni: String,
    },
}

// ─── Client Config ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// 服务端地址，如 "example.com:7000"
    /// quic 模式下是 UDP 端口，tcp 模式下是 TCP+TLS+HTTP/2 端口
    pub server_addr: String,

    /// 传输模式：quic / tcp，默认 quic
    #[serde(default)]
    pub transport: ClientTransport,

    /// 认证 token
    #[serde(default)]
    pub token: String,

    /// 是否跳过服务端证书校验（自签名 + tls_skip_verify=true 时使用）
    #[serde(default)]
    pub tls_skip_verify: bool,

    /// TLS SNI 域名（可选）
    /// 服务端使用正规 CA 证书时，填写证书对应的域名
    /// 不填则从 server_addr 中提取 hostname 作为 SNI
    #[serde(default)]
    pub tls_sni: Option<String>,

    /// 心跳间隔（秒）
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,

    /// 断线重连最大间隔（秒）
    #[serde(default = "default_reconnect_max")]
    pub reconnect_max_secs: u64,

    /// 日志等级，如 "info"、"debug"、"warn"、"error"
    /// 也支持细粒度写法，如 "tunx=debug,quinn=warn"
    /// 可被环境变量 RUST_LOG 覆盖
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// QUIC 传输层调优
    #[serde(default)]
    pub quic: QuicConfig,

    /// WebSocket 路径（仅 websocket 模式生效）
    /// 默认 "/tunx-ws"
    #[serde(default = "default_ws_path")]
    pub ws_path: String,

    /// 代理列表
    pub proxies: Vec<ProxyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// 代理名称，session 内唯一
    pub name: String,

    #[serde(flatten)]
    pub kind: ProxyKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProxyKind {
    Tcp(TcpProxyConfig),
    Udp(UdpProxyConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpProxyConfig {
    /// 本地服务地址，如 "127.0.0.1:8080"
    pub local_addr: String,
    /// 服务端暴露端口，0 = 随机分配
    #[serde(default)]
    pub remote_port: u16,

    /// 是否启用服务端 TLS 终止：true 时该端口只能 https 访问
    /// 明文 HTTP 请求会被 301 跳转到 https
    /// 仅在服务端使用正规 CA 签发证书（acme / manual）时可用
    #[serde(default)]
    pub tls: bool,

    /// tls=true 时必填，访问该端口使用的域名
    /// 必须与服务端证书覆盖的域名（SAN）匹配
    #[serde(default)]
    pub custom_domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpProxyConfig {
    /// 本地服务地址，如 "127.0.0.1:53"
    pub local_addr: String,
    /// 服务端暴露端口，0 = 随机分配
    #[serde(default)]
    pub remote_port: u16,
}

// ─── defaults ─────────────────────────────────────────────────────────────────

fn default_bind_addr() -> String {
    "0.0.0.0:7000".to_string()
}
fn default_port_range() -> (u16, u16) {
    (10000, 20000)
}
fn default_heartbeat_timeout() -> u64 {
    90
}
fn default_heartbeat_interval() -> u64 {
    30
}
fn default_reconnect_max() -> u64 {
    60
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_acme_cache() -> PathBuf {
    PathBuf::from("./acme-cache")
}
fn default_self_signed_sni() -> String {
    "www.bing.com".to_string()
}
fn default_ws_path() -> String {
    "/tunx-ws".to_string()
}

// ─── helpers ──────────────────────────────────────────────────────────────────

impl TunxConfig {
    /// 严格加载：文件必须存在且必须能解析为合法的 server/client 配置。
    /// 用于确认"本次要真正跑 server/client 逻辑"的场景。
    pub fn from_file(path: &str) -> crate::Result<Self> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| crate::TunxError::Config(format!("read {path}: {e}")))?;
        let cfg: TunxConfig = toml::from_str(&s)
            .map_err(|e| crate::TunxError::Config(format!("parse {path}: {e}")))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// 宽松加载：只要求 TOML 语法正确、mode 字段合法即可，
    /// 不要求 [server]/[client] 段完整。用于 Web UI 读取"待编辑"的配置。
    pub fn from_file_loose(path: &str) -> crate::Result<Self> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| crate::TunxError::Config(format!("read {path}: {e}")))?;
        let cfg: TunxConfig = toml::from_str(&s)
            .map_err(|e| crate::TunxError::Config(format!("parse {path}: {e}")))?;
        Ok(cfg)
    }

    /// 是否已经是一份"可以直接跑"的完整配置（server/client 段齐全且通过校验）
    pub fn is_runnable(&self) -> bool {
        self.validate().is_ok()
    }

    /// 写回 TOML 文件（保存配置 / 首次生成空模板时使用）
    pub fn save_to_file(&self, path: &str) -> crate::Result<()> {
        let s = toml::to_string_pretty(self)
            .map_err(|e| crate::TunxError::Config(format!("serialize config: {e}")))?;
        std::fs::write(path, s)
            .map_err(|e| crate::TunxError::Config(format!("write {path}: {e}")))?;
        Ok(())
    }

    /// 生成一份最小可用的空白模板：mode 默认为 server，但 [server] 段留空的必填项
    /// （token 等）为占位符，需要用户在 Web UI 里补全后才算 `is_runnable() == true`。
    pub fn empty_template() -> Self {
        Self {
            mode: Mode::Server,
            server: None,
            client: None,
            web: WebConfig::default(),
        }
    }

    pub fn validate(&self) -> crate::Result<()> {
        match self.mode {
            Mode::Server => {
                let server = self.server.as_ref().ok_or_else(|| {
                    crate::TunxError::Config("mode=server but [server] section is missing".into())
                })?;
                server.validate()?;
            }
            Mode::Client => {
                let client = self.client.as_ref().ok_or_else(|| {
                    crate::TunxError::Config("mode=client but [client] section is missing".into())
                })?;
                client.validate()?;
            }
        }
        Ok(())
    }
}

impl ServerConfig {
    /// 校验：tcp/websocket/xhttp 模式不允许使用 self_signed 证书
    fn validate(&self) -> crate::Result<()> {
        if self.transport.needs_public_tls()
            && matches!(self.tls, ServerTlsConfig::SelfSigned { .. })
        {
            return Err(crate::TunxError::Config(format!(
                "transport={:?} requires acme or manual TLS; \
                 self_signed is not supported in TCP/WebSocket/XHTTP mode",
                self.transport
            )));
        }
        Ok(())
    }
}

impl ClientConfig {
    /// 校验：tls=true 时 custom_domain 必填
    fn validate(&self) -> crate::Result<()> {
        for p in &self.proxies {
            if let ProxyKind::Tcp(t) = &p.kind {
                if t.tls {
                    match &t.custom_domain {
                        Some(d) if !d.trim().is_empty() => {}
                        _ => {
                            return Err(crate::TunxError::Config(format!(
                                "proxy '{}' has tls=true but custom_domain is empty; \
                                 it must be set and must match the server certificate's SAN",
                                p.name
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
