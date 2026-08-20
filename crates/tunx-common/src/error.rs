use thiserror::Error;

pub type Result<T> = std::result::Result<T, TunxError>;

#[derive(Debug, Error)]
pub enum TunxError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("QUIC connection error: {0}")]
    QuicConnect(#[from] quinn::ConnectError),

    #[error("QUIC connection closed: {0}")]
    QuicConnection(#[from] quinn::ConnectionError),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Auth failed: {0}")]
    AuthFailed(String),

    #[error("Proxy error: {0}")]
    Proxy(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
