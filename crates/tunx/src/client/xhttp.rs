//! XHTTP 传输层
//!
//! TLS + HTTP/2 + gRPC，技术实现与 TCP 模式相同
//! 设计目标：通过 CDN 代理使用标准 HTTPS 端口（443）连接
//!
//! 控制流和数据流均复用 TCP 路径的 run_session_tcp / handle_work_conn_tcp
