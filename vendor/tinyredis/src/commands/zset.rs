use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Mutex;

use crate::commands::utils::{aof_log, parse_i64, to_str};
use crate::error::{Error, Result};
use crate::parser::{Command, Frame};
use crate::persistence::AofSender;
use crate::store::{LexBound, ScoreBound, Store, ZAddOpts};

type SharedStore = Arc<Mutex<Store>>;

pub fn parse_score(b: &Bytes) -> Result<f64> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Protocol("ERR not a float or out of range".into()))?;
    match s {
        "+inf" | "inf" => Ok(f64::INFINITY),
        "-inf" => Ok(f64::NEG_INFINITY),
        _ => {
            let f: f64 = s
                .parse()
                .map_err(|_| Error::Protocol("ERR not a float or out of range".into()))?;
            if f.is_nan() {
                Err(Error::Protocol("ERR not a float or out of range".into()))
            } else {
                Ok(f)
            }
        }
    }
}

pub fn format_score(score: f64) -> String {
    if score == f64::INFINITY {
        "inf".to_string()
    } else if score == f64::NEG_INFINITY {
        "-inf".to_string()
    } else {
        format!("{score}")
    }
}

/// Parse a score bound string like `(3.5`, `3.5`, `+inf`, `-inf`.
fn parse_score_bound(b: &Bytes) -> Result<ScoreBound> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Protocol("ERR min or max is not a float".into()))?;
    if let Some(rest) = s.strip_prefix('(') {
        let f = rest
            .parse::<f64>()
            .map_err(|_| Error::Protocol("ERR min or max is not a float".into()))?;
        Ok(ScoreBound::Exclusive(f))
    } else {
        match s {
            "+inf" | "inf" => Ok(ScoreBound::PosInf),
            "-inf" => Ok(ScoreBound::NegInf),
            _ => {
                let f = s
                    .parse::<f64>()
                    .map_err(|_| Error::Protocol("ERR min or max is not a float".into()))?;
                Ok(ScoreBound::Inclusive(f))
            }
        }
    }
}

/// Parse a lex bound like `[alpha`, `(alpha`, `-`, `+`.
fn parse_lex_bound(b: &Bytes) -> Result<LexBound> {
    let s = std::str::from_utf8(b)
        .map_err(|_| Error::Protocol("ERR min or max is not valid".into()))?;
    match s {
        "-" => Ok(LexBound::NegInf),
        "+" => Ok(LexBound::PosInf),
        _ if s.starts_with('[') => Ok(LexBound::Inclusive(Bytes::copy_from_slice(
            &s.as_bytes()[1..],
        ))),
        _ if s.starts_with('(') => Ok(LexBound::Exclusive(Bytes::copy_from_slice(
            &s.as_bytes()[1..],
        ))),
        _ => Err(Error::Protocol(
            "ERR min or max is not valid string range item".into(),
        )),
    }
}

/// Parse LIMIT offset count from args at position `pos`.
fn parse_limit(args: &[Bytes], pos: usize) -> Result<Option<(usize, usize)>> {
    if pos + 2 < args.len() && args[pos].to_ascii_uppercase() == b"LIMIT" {
        let offset = parse_i64(&args[pos + 1])? as usize;
        let count_val = parse_i64(&args[pos + 2])?;
        let count = if count_val < 0 {
            usize::MAX
        } else {
            count_val as usize
        };
        return Ok(Some((offset, count)));
    }
    Ok(None)
}

fn withscores_frame(items: Vec<(String, f64)>) -> Frame {
    Frame::Array(
        items
            .into_iter()
            .flat_map(|(m, s)| {
                [
                    Frame::Bulk(Bytes::from(m)),
                    Frame::Bulk(Bytes::from(format_score(s))),
                ]
            })
            .collect(),
    )
}

fn members_frame(items: Vec<(String, f64)>) -> Frame {
    Frame::Array(
        items
            .into_iter()
            .map(|(m, _)| Frame::Bulk(Bytes::from(m)))
            .collect(),
    )
}

pub async fn zadd(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    // ZADD key [NX|XX] [GT|LT] [CH] [INCR] score member [score member ...]
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZADD"));
    }
    let key = to_str(cmd.arg(0, "ZADD")?, "ZADD")?;

    let mut opts = ZAddOpts::default();
    let mut idx = 1usize;
    loop {
        let flag = cmd.args.get(idx).map(|b| b.to_ascii_uppercase());
        match flag.as_deref() {
            Some(b"NX") => {
                opts.nx = true;
                idx += 1;
            }
            Some(b"XX") => {
                opts.xx = true;
                idx += 1;
            }
            Some(b"GT") => {
                opts.gt = true;
                idx += 1;
            }
            Some(b"LT") => {
                opts.lt = true;
                idx += 1;
            }
            Some(b"CH") => {
                opts.ch = true;
                idx += 1;
            }
            Some(b"INCR") => {
                opts.incr = true;
                idx += 1;
            }
            _ => break,
        }
    }

    // Parse score-member pairs
    let remaining = &cmd.args[idx..];
    if remaining.len() < 2 || !remaining.len().is_multiple_of(2) {
        return Err(Error::WrongArity("ZADD"));
    }
    let mut entries: Vec<(f64, String)> = Vec::new();
    let mut i = 0;
    while i < remaining.len() {
        let score = parse_score(&remaining[i])?;
        let member = to_str(&remaining[i + 1], "ZADD")?;
        entries.push((score, member));
        i += 2;
    }

    if opts.incr {
        if entries.len() != 1 {
            return Err(Error::Protocol(
                "ERR INCR option supports a single increment-element pair".into(),
            ));
        }
        let (delta, ref member) = entries[0];
        let new_score = store.lock().await.zincrby(&key, member, delta)?;
        let score_str = format_score(new_score);
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"ZADD")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(score_str.clone())),
                Frame::Bulk(Bytes::from(member.clone())),
            ]),
        )
        .await;
        return Ok(Frame::Bulk(Bytes::from(score_str)));
    }

    let count = store.lock().await.zadd(&key, &entries, &opts)?;

    // Log as ZADD key score member [...]
    let mut aof_parts = vec![
        Frame::Bulk(Bytes::from_static(b"ZADD")),
        Frame::Bulk(Bytes::from(key)),
    ];
    for (score, member) in &entries {
        aof_parts.push(Frame::Bulk(Bytes::from(format_score(*score))));
        aof_parts.push(Frame::Bulk(Bytes::from(member.clone())));
    }
    aof_log(aof, Frame::Array(aof_parts)).await;

    Ok(Frame::Integer(count))
}

pub async fn zrem(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("ZREM"));
    }
    let key = to_str(cmd.arg(0, "ZREM")?, "ZREM")?;
    let members: Vec<String> = cmd.args[1..]
        .iter()
        .map(|b| to_str(b, "ZREM"))
        .collect::<Result<_>>()?;

    let removed = store.lock().await.zrem(&key, &members)?;

    if removed > 0 {
        let mut parts = vec![
            Frame::Bulk(Bytes::from_static(b"ZREM")),
            Frame::Bulk(Bytes::from(key)),
        ];
        parts.extend(members.into_iter().map(|m| Frame::Bulk(Bytes::from(m))));
        aof_log(aof, Frame::Array(parts)).await;
    }
    Ok(Frame::Integer(removed as i64))
}

pub async fn zremrangebyrank(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    if cmd.args.len() != 3 {
        return Err(Error::WrongArity("ZREMRANGEBYRANK"));
    }
    let key = to_str(cmd.arg(0, "ZREMRANGEBYRANK")?, "ZREMRANGEBYRANK")?;
    let start = parse_i64(cmd.arg(1, "ZREMRANGEBYRANK")?)?;
    let stop = parse_i64(cmd.arg(2, "ZREMRANGEBYRANK")?)?;
    let count = store.lock().await.zremrangebyrank(&key, start, stop)?;
    if count > 0 {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"ZREMRANGEBYRANK")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(start.to_string())),
                Frame::Bulk(Bytes::from(stop.to_string())),
            ]),
        )
        .await;
    }
    Ok(Frame::Integer(count as i64))
}

pub async fn zscore(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("ZSCORE"));
    }
    let key = to_str(cmd.arg(0, "ZSCORE")?, "ZSCORE")?;
    let member = to_str(cmd.arg(1, "ZSCORE")?, "ZSCORE")?;
    match store.lock().await.zscore(&key, &member)? {
        None => Ok(Frame::Null),
        Some(s) => Ok(Frame::Bulk(Bytes::from(format_score(s)))),
    }
}

pub async fn zincrby(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZINCRBY"));
    }
    let key = to_str(cmd.arg(0, "ZINCRBY")?, "ZINCRBY")?;
    let delta = parse_score(cmd.arg(1, "ZINCRBY")?)?;
    let member = to_str(cmd.arg(2, "ZINCRBY")?, "ZINCRBY")?;

    let new_score = store.lock().await.zincrby(&key, &member, delta)?;
    let score_str = format_score(new_score);

    // Canonicalize as ZADD key score member
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"ZADD")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(Bytes::from(score_str.clone())),
            Frame::Bulk(Bytes::from(member)),
        ]),
    )
    .await;
    Ok(Frame::Bulk(Bytes::from(score_str)))
}

pub async fn zrank(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("ZRANK"));
    }
    let key = to_str(cmd.arg(0, "ZRANK")?, "ZRANK")?;
    let member = to_str(cmd.arg(1, "ZRANK")?, "ZRANK")?;
    match store.lock().await.zrank(&key, &member)? {
        None => Ok(Frame::Null),
        Some(r) => Ok(Frame::Integer(r as i64)),
    }
}

pub async fn zrevrank(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 2 {
        return Err(Error::WrongArity("ZREVRANK"));
    }
    let key = to_str(cmd.arg(0, "ZREVRANK")?, "ZREVRANK")?;
    let member = to_str(cmd.arg(1, "ZREVRANK")?, "ZREVRANK")?;
    match store.lock().await.zrevrank(&key, &member)? {
        None => Ok(Frame::Null),
        Some(r) => Ok(Frame::Integer(r as i64)),
    }
}

pub async fn zcard(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    let key = to_str(cmd.arg(0, "ZCARD")?, "ZCARD")?;
    Ok(Frame::Integer(store.lock().await.zcard(&key)? as i64))
}

pub async fn zcount(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZCOUNT"));
    }
    let key = to_str(cmd.arg(0, "ZCOUNT")?, "ZCOUNT")?;
    let min = parse_score_bound(cmd.arg(1, "ZCOUNT")?)?;
    let max = parse_score_bound(cmd.arg(2, "ZCOUNT")?)?;
    Ok(Frame::Integer(
        store.lock().await.zcount(&key, &min, &max)? as i64
    ))
}

pub async fn zrange(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    // ZRANGE key min max [BYSCORE|BYLEX] [REV] [LIMIT offset count] [WITHSCORES]
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZRANGE"));
    }
    let key = to_str(cmd.arg(0, "ZRANGE")?, "ZRANGE")?;

    // Scan optional flags
    let mut byscore = false;
    let mut bylex = false;
    let mut rev = false;
    let mut with_scores = false;
    let mut limit: Option<(usize, usize)> = None;
    let mut flag_idx = 3usize;
    while flag_idx < cmd.args.len() {
        match cmd.args[flag_idx].to_ascii_uppercase().as_slice() {
            b"BYSCORE" => {
                byscore = true;
                flag_idx += 1;
            }
            b"BYLEX" => {
                bylex = true;
                flag_idx += 1;
            }
            b"REV" => {
                rev = true;
                flag_idx += 1;
            }
            b"WITHSCORES" => {
                with_scores = true;
                flag_idx += 1;
            }
            b"LIMIT" => {
                if flag_idx + 2 >= cmd.args.len() {
                    return Err(Error::Protocol("ERR syntax error".into()));
                }
                let offset = parse_i64(&cmd.args[flag_idx + 1])? as usize;
                let count_val = parse_i64(&cmd.args[flag_idx + 2])?;
                let count = if count_val < 0 {
                    usize::MAX
                } else {
                    count_val as usize
                };
                limit = Some((offset, count));
                flag_idx += 3;
            }
            _ => flag_idx += 1,
        }
    }

    let mut locked = store.lock().await;
    if byscore {
        let (min, max) = if rev {
            (
                parse_score_bound(cmd.arg(2, "ZRANGE")?)?,
                parse_score_bound(cmd.arg(1, "ZRANGE")?)?,
            )
        } else {
            (
                parse_score_bound(cmd.arg(1, "ZRANGE")?)?,
                parse_score_bound(cmd.arg(2, "ZRANGE")?)?,
            )
        };
        let items = locked.zrange_by_score(&key, &min, &max, rev, limit)?;
        if with_scores {
            Ok(withscores_frame(items))
        } else {
            Ok(members_frame(items))
        }
    } else if bylex {
        let (min, max) = if rev {
            (
                parse_lex_bound(cmd.arg(2, "ZRANGE")?)?,
                parse_lex_bound(cmd.arg(1, "ZRANGE")?)?,
            )
        } else {
            (
                parse_lex_bound(cmd.arg(1, "ZRANGE")?)?,
                parse_lex_bound(cmd.arg(2, "ZRANGE")?)?,
            )
        };
        let members = locked.zrange_by_lex(&key, &min, &max, rev, limit)?;
        Ok(Frame::Array(
            members
                .into_iter()
                .map(|m| Frame::Bulk(Bytes::from(m)))
                .collect(),
        ))
    } else {
        let start = parse_i64(cmd.arg(1, "ZRANGE")?)?;
        let stop = parse_i64(cmd.arg(2, "ZRANGE")?)?;
        let items = locked.zrange_by_index(&key, start, stop, rev)?;
        if with_scores {
            Ok(withscores_frame(items))
        } else {
            Ok(members_frame(items))
        }
    }
}

pub async fn zrangebyscore(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    // ZRANGEBYSCORE key min max [WITHSCORES] [LIMIT offset count]
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZRANGEBYSCORE"));
    }
    let key = to_str(cmd.arg(0, "ZRANGEBYSCORE")?, "ZRANGEBYSCORE")?;
    let min = parse_score_bound(cmd.arg(1, "ZRANGEBYSCORE")?)?;
    let max = parse_score_bound(cmd.arg(2, "ZRANGEBYSCORE")?)?;

    let with_scores = cmd.args[3..]
        .iter()
        .any(|b| b.to_ascii_uppercase() == b"WITHSCORES");
    let limit = parse_limit(
        &cmd.args,
        cmd.args
            .iter()
            .position(|b| b.to_ascii_uppercase() == b"LIMIT")
            .unwrap_or(usize::MAX),
    )?;

    let items = store
        .lock()
        .await
        .zrange_by_score(&key, &min, &max, false, limit)?;
    if with_scores {
        Ok(withscores_frame(items))
    } else {
        Ok(members_frame(items))
    }
}

pub async fn zrevrangebyscore(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    // ZREVRANGEBYSCORE key max min [WITHSCORES] [LIMIT offset count]
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZREVRANGEBYSCORE"));
    }
    let key = to_str(cmd.arg(0, "ZREVRANGEBYSCORE")?, "ZREVRANGEBYSCORE")?;
    // Note: max before min for ZREVRANGEBYSCORE
    let max = parse_score_bound(cmd.arg(1, "ZREVRANGEBYSCORE")?)?;
    let min = parse_score_bound(cmd.arg(2, "ZREVRANGEBYSCORE")?)?;

    let with_scores = cmd.args[3..]
        .iter()
        .any(|b| b.to_ascii_uppercase() == b"WITHSCORES");
    let limit = parse_limit(
        &cmd.args,
        cmd.args
            .iter()
            .position(|b| b.to_ascii_uppercase() == b"LIMIT")
            .unwrap_or(usize::MAX),
    )?;

    let items = store
        .lock()
        .await
        .zrange_by_score(&key, &min, &max, true, limit)?;
    if with_scores {
        Ok(withscores_frame(items))
    } else {
        Ok(members_frame(items))
    }
}

pub async fn zrangebylex(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    // ZRANGEBYLEX key min max [LIMIT offset count]
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZRANGEBYLEX"));
    }
    let key = to_str(cmd.arg(0, "ZRANGEBYLEX")?, "ZRANGEBYLEX")?;
    let min = parse_lex_bound(cmd.arg(1, "ZRANGEBYLEX")?)?;
    let max = parse_lex_bound(cmd.arg(2, "ZRANGEBYLEX")?)?;

    let limit = parse_limit(
        &cmd.args,
        cmd.args
            .iter()
            .position(|b| b.to_ascii_uppercase() == b"LIMIT")
            .unwrap_or(usize::MAX),
    )?;

    let members = store
        .lock()
        .await
        .zrange_by_lex(&key, &min, &max, false, limit)?;
    Ok(Frame::Array(
        members
            .into_iter()
            .map(|m| Frame::Bulk(Bytes::from(m)))
            .collect(),
    ))
}

pub async fn zpopmin(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("ZPOPMIN"));
    }
    let key = to_str(cmd.arg(0, "ZPOPMIN")?, "ZPOPMIN")?;
    let count = if cmd.args.len() > 1 {
        parse_i64(cmd.arg(1, "ZPOPMIN")?)? as usize
    } else {
        1
    };

    let items = store.lock().await.zpopmin(&key, count)?;
    if !items.is_empty() {
        let mut parts = vec![
            Frame::Bulk(Bytes::from_static(b"ZREM")),
            Frame::Bulk(Bytes::from(key)),
        ];
        parts.extend(
            items
                .iter()
                .map(|(m, _)| Frame::Bulk(Bytes::from(m.clone()))),
        );
        aof_log(aof, Frame::Array(parts)).await;
    }
    Ok(withscores_frame(items))
}

pub async fn zpopmax(cmd: &Command, store: &SharedStore, aof: &Option<AofSender>) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("ZPOPMAX"));
    }
    let key = to_str(cmd.arg(0, "ZPOPMAX")?, "ZPOPMAX")?;
    let count = if cmd.args.len() > 1 {
        parse_i64(cmd.arg(1, "ZPOPMAX")?)? as usize
    } else {
        1
    };

    let items = store.lock().await.zpopmax(&key, count)?;
    if !items.is_empty() {
        let mut parts = vec![
            Frame::Bulk(Bytes::from_static(b"ZREM")),
            Frame::Bulk(Bytes::from(key)),
        ];
        parts.extend(
            items
                .iter()
                .map(|(m, _)| Frame::Bulk(Bytes::from(m.clone()))),
        );
        aof_log(aof, Frame::Array(parts)).await;
    }
    Ok(withscores_frame(items))
}

pub async fn zrandmember(cmd: &Command, store: &SharedStore) -> Result<Frame> {
    if cmd.args.is_empty() {
        return Err(Error::WrongArity("ZRANDMEMBER"));
    }
    let key = to_str(cmd.arg(0, "ZRANDMEMBER")?, "ZRANDMEMBER")?;

    if cmd.args.len() == 1 {
        // Single random member
        let members = store.lock().await.zrandmember(&key, 1)?;
        return match members.into_iter().next() {
            None => Ok(Frame::Null),
            Some(m) => Ok(Frame::Bulk(Bytes::from(m))),
        };
    }

    let count = parse_i64(cmd.arg(1, "ZRANDMEMBER")?)?;
    let with_scores = cmd
        .args
        .get(2)
        .is_some_and(|b| b.to_ascii_uppercase() == b"WITHSCORES");

    let members = store.lock().await.zrandmember(&key, count)?;
    if with_scores {
        let mut locked = store.lock().await;
        let scores: Vec<(String, f64)> = members
            .into_iter()
            .map(|m| {
                let s = locked.zscore(&key, &m).ok().flatten().unwrap_or(0.0);
                (m, s)
            })
            .collect();
        Ok(withscores_frame(scores))
    } else {
        Ok(Frame::Array(
            members
                .into_iter()
                .map(|m| Frame::Bulk(Bytes::from(m)))
                .collect(),
        ))
    }
}

pub async fn zunionstore(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZUNIONSTORE"));
    }
    let dest = to_str(cmd.arg(0, "ZUNIONSTORE")?, "ZUNIONSTORE")?;
    let num_keys = parse_i64(cmd.arg(1, "ZUNIONSTORE")?)? as usize;
    if cmd.args.len() < 2 + num_keys {
        return Err(Error::WrongArity("ZUNIONSTORE"));
    }
    let keys: Vec<String> = cmd.args[2..2 + num_keys]
        .iter()
        .map(|b| to_str(b, "ZUNIONSTORE"))
        .collect::<Result<_>>()?;

    let count = store.lock().await.zunionstore(&dest, &keys)?;

    aof_log_store_result(aof, &dest, store, count).await;
    Ok(Frame::Integer(count as i64))
}

pub async fn zinterstore(
    cmd: &Command,
    store: &SharedStore,
    aof: &Option<AofSender>,
) -> Result<Frame> {
    if cmd.args.len() < 3 {
        return Err(Error::WrongArity("ZINTERSTORE"));
    }
    let dest = to_str(cmd.arg(0, "ZINTERSTORE")?, "ZINTERSTORE")?;
    let num_keys = parse_i64(cmd.arg(1, "ZINTERSTORE")?)? as usize;
    if cmd.args.len() < 2 + num_keys {
        return Err(Error::WrongArity("ZINTERSTORE"));
    }
    let keys: Vec<String> = cmd.args[2..2 + num_keys]
        .iter()
        .map(|b| to_str(b, "ZINTERSTORE"))
        .collect::<Result<_>>()?;

    let count = store.lock().await.zinterstore(&dest, &keys)?;

    aof_log_store_result(aof, &dest, store, count).await;
    Ok(Frame::Integer(count as i64))
}

/// After ZUNIONSTORE/ZINTERSTORE, log as DEL dest + ZADD dest score member [...].
async fn aof_log_store_result(
    aof: &Option<AofSender>,
    dest: &str,
    store: &SharedStore,
    count: usize,
) {
    if count == 0 {
        aof_log(
            aof,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"DEL")),
                Frame::Bulk(Bytes::from(dest.to_string())),
            ]),
        )
        .await;
        return;
    }
    aof_log(
        aof,
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"DEL")),
            Frame::Bulk(Bytes::from(dest.to_string())),
        ]),
    )
    .await;
    // Snapshot current dest content
    let items: Vec<(String, f64)> = {
        let mut locked = store.lock().await;
        locked
            .zrange_by_index(dest, 0, -1, false)
            .unwrap_or_default()
    };
    if !items.is_empty() {
        let mut parts = vec![
            Frame::Bulk(Bytes::from_static(b"ZADD")),
            Frame::Bulk(Bytes::from(dest.to_string())),
        ];
        for (m, s) in items {
            parts.push(Frame::Bulk(Bytes::from(format_score(s))));
            parts.push(Frame::Bulk(Bytes::from(m)));
        }
        aof_log(aof, Frame::Array(parts)).await;
    }
}
