use crate::error::{Error, Result};
use crate::parser::Frame;
use crate::persistence::{AofEntry, AofSender};
use bytes::Bytes;

pub fn parse_u64(b: &Bytes) -> Result<u64> {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::NotInteger)
}

pub fn to_str(b: &Bytes, cmd: &'static str) -> Result<String> {
    std::str::from_utf8(b)
        .map(String::from)
        .map_err(|_| Error::WrongArity(cmd))
}

pub async fn aof_log(aof: &Option<AofSender>, frame: Frame) {
    if let Some(tx) = aof {
        let _ = tx
            .send(AofEntry {
                raw: frame.serialize(),
            })
            .await;
    }
}

pub fn parse_i64(b: &Bytes) -> Result<i64> {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::NotInteger)
}
