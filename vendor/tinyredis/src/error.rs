#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("WRONGTYPE Operation against a key holding the wrong kind of value")]
    WrongType,

    #[error("value is not an integer or out of range")]
    NotInteger,

    #[error("wrong number of arguments for '{0}' command")]
    WrongArity(&'static str),

    #[error("syntax error")]
    Syntax,

    #[error("server is shutting down")]
    Shutdown,
}

pub type Result<T> = std::result::Result<T, Error>;
