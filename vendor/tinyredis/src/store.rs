use crate::error::{Error, Result};
use bytes::Bytes;
use ordered_float::OrderedFloat;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

fn glob_match(pattern: &[char], s: &[char]) -> bool {
    match pattern {
        [] => s.is_empty(),
        ['*', rest @ ..] => glob_match(rest, s) || (!s.is_empty() && glob_match(pattern, &s[1..])),
        ['?', rest @ ..] => !s.is_empty() && glob_match(rest, &s[1..]),
        [c, rest @ ..] => !s.is_empty() && s[0] == *c && glob_match(rest, &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new()
    }

    fn str_val(s: &'static str) -> Value {
        Value::Str(Bytes::from_static(s.as_bytes()))
    }

    #[test]
    fn set_and_get() {
        let mut s = store();
        s.set("k", str_val("v"), None);
        match s.get("k").map(|e| &e.value) {
            Some(Value::Str(b)) => assert_eq!(b.as_ref(), b"v"),
            _ => panic!("expected Str"),
        }
    }

    #[test]
    fn get_missing_returns_none() {
        let mut s = store();
        assert!(s.get("nope").is_none());
    }

    #[test]
    fn get_expired_returns_none_and_removes_key() {
        let mut s = store();
        s.set("k", str_val("v"), Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        assert!(s.get("k").is_none());
        // key was lazily removed — raw map should be empty
        assert!(!s.data.contains_key("k"));
    }

    #[test]
    fn overwrite_updates_value() {
        let mut s = store();
        s.set("k", str_val("first"), None);
        s.set("k", str_val("second"), None);
        match s.get("k").map(|e| &e.value) {
            Some(Value::Str(b)) => assert_eq!(b.as_ref(), b"second"),
            _ => panic!(),
        }
    }

    #[test]
    fn del_existing_keys() {
        let mut s = store();
        s.set("a", str_val("1"), None);
        s.set("b", str_val("2"), None);
        let count = s.del(&["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(count, 2);
        assert!(s.get("a").is_none());
    }

    #[test]
    fn del_returns_zero_for_missing() {
        let mut s = store();
        assert_eq!(s.del(&["ghost".to_string()]), 0);
    }

    #[test]
    fn exists_counts_present_keys() {
        let mut s = store();
        s.set("x", str_val("1"), None);
        assert_eq!(s.exists(&["x".to_string(), "y".to_string()]), 1);
    }

    #[test]
    fn exists_counts_duplicate_args() {
        let mut s = store();
        s.set("x", str_val("1"), None);
        assert_eq!(s.exists(&["x".to_string(), "x".to_string()]), 2);
    }

    #[test]
    fn exists_excludes_expired_keys() {
        let mut s = store();
        s.set("x", str_val("1"), Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(s.exists(&["x".to_string()]), 0);
    }

    #[test]
    fn ttl_no_expiry_returns_minus_one() {
        let mut s = store();
        s.set("k", str_val("v"), None);
        assert_eq!(s.ttl("k"), -1);
    }

    #[test]
    fn ttl_missing_key_returns_minus_two() {
        let mut s = store();
        assert_eq!(s.ttl("ghost"), -2);
    }

    #[test]
    fn ttl_with_expiry_returns_positive() {
        let mut s = store();
        s.set("k", str_val("v"), Some(Duration::from_secs(10)));
        let ttl = s.ttl("k");
        assert!(ttl > 0 && ttl <= 10, "got {ttl}");
    }

    #[test]
    fn ttl_rounds_up_sub_second_remainder() {
        let mut s = store();
        s.set("k", str_val("v"), Some(Duration::from_millis(900)));
        assert_eq!(s.ttl("k"), 1);
    }

    // ── persist ───────────────────────────────────────────────────────────────

    #[test]
    fn persist_removes_ttl() {
        let mut s = store();
        s.set("k", str_val("v"), Some(Duration::from_secs(10)));
        assert!(s.persist("k"));
        assert_eq!(s.ttl("k"), -1);
    }

    #[test]
    fn persist_returns_false_when_no_ttl() {
        let mut s = store();
        s.set("k", str_val("v"), None);
        assert!(!s.persist("k"));
    }

    #[test]
    fn persist_returns_false_for_missing_key() {
        let mut s = store();
        assert!(!s.persist("ghost"));
    }

    // ── pttl ──────────────────────────────────────────────────────────────────

    #[test]
    fn pttl_missing_key_returns_minus_two() {
        let mut s = store();
        assert_eq!(s.pttl("ghost"), -2);
    }

    #[test]
    fn pttl_no_expiry_returns_minus_one() {
        let mut s = store();
        s.set("k", str_val("v"), None);
        assert_eq!(s.pttl("k"), -1);
    }

    #[test]
    fn pttl_returns_milliseconds() {
        let mut s = store();
        s.set("k", str_val("v"), Some(Duration::from_millis(5000)));
        let ms = s.pttl("k");
        assert!(ms > 0 && ms <= 5000, "got {ms}");
    }

    #[test]
    fn pttl_rounds_up_sub_millisecond_remainder() {
        let mut s = store();
        s.set("k", str_val("v"), Some(Duration::from_micros(500)));
        assert_eq!(s.pttl("k"), 1);
    }

    // ── keys / dbsize / rename / flushdb ──────────────────────────────────────

    #[test]
    fn keys_star_returns_all() {
        let mut s = store();
        s.set("a", str_val("1"), None);
        s.set("b", str_val("2"), None);
        let mut k = s.keys("*");
        k.sort();
        assert_eq!(k, vec!["a", "b"]);
    }

    #[test]
    fn keys_prefix_wildcard() {
        let mut s = store();
        s.set("foo:1", str_val("v"), None);
        s.set("foo:2", str_val("v"), None);
        s.set("bar:1", str_val("v"), None);
        let mut k = s.keys("foo:*");
        k.sort();
        assert_eq!(k, vec!["foo:1", "foo:2"]);
    }

    #[test]
    fn keys_question_mark_wildcard() {
        let mut s = store();
        s.set("he", str_val("v"), None);
        s.set("hello", str_val("v"), None);
        let k = s.keys("h?");
        assert_eq!(k, vec!["he"]);
    }

    #[test]
    fn keys_excludes_expired() {
        let mut s = store();
        s.set("live", str_val("v"), None);
        s.set("dead", str_val("v"), Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        let k = s.keys("*");
        assert_eq!(k, vec!["live"]);
    }

    #[test]
    fn dbsize_counts_non_expired() {
        let mut s = store();
        s.set("a", str_val("v"), None);
        s.set("b", str_val("v"), Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(s.dbsize(), 1);
    }

    #[test]
    fn rename_moves_key() {
        let mut s = store();
        s.set("src", str_val("v"), None);
        s.rename("src", "dst").unwrap();
        assert!(s.get("src").is_none());
        assert!(s.get("dst").is_some());
    }

    #[test]
    fn rename_preserves_ttl() {
        let mut s = store();
        s.set("src", str_val("v"), Some(Duration::from_secs(10)));
        s.rename("src", "dst").unwrap();
        let ttl = s.ttl("dst");
        assert!(ttl > 0 && ttl <= 10);
    }

    #[test]
    fn rename_missing_key_errors() {
        let mut s = store();
        assert!(s.rename("ghost", "dst").is_err());
    }

    #[test]
    fn flushdb_clears_all_keys() {
        let mut s = store();
        s.set("a", str_val("1"), None);
        s.set("b", str_val("2"), None);
        s.flushdb();
        assert_eq!(s.dbsize(), 0);
    }

    // ── glob_match ────────────────────────────────────────────────────────────

    #[test]
    fn glob_star_matches_anything() {
        let p: Vec<char> = "*".chars().collect();
        assert!(glob_match(&p, &"hello".chars().collect::<Vec<_>>()));
        assert!(glob_match(&p, &"".chars().collect::<Vec<_>>()));
    }

    #[test]
    fn glob_question_matches_one_char() {
        let p: Vec<char> = "h?llo".chars().collect();
        assert!(glob_match(&p, &"hello".chars().collect::<Vec<_>>()));
        assert!(!glob_match(&p, &"hllo".chars().collect::<Vec<_>>()));
    }

    #[test]
    fn glob_literal_match() {
        let p: Vec<char> = "foo".chars().collect();
        assert!(glob_match(&p, &"foo".chars().collect::<Vec<_>>()));
        assert!(!glob_match(&p, &"bar".chars().collect::<Vec<_>>()));
    }

    // ── strlen / getdel / getset / incr_by_float ──────────────────────────────

    #[test]
    fn strlen_existing_key() {
        let mut s = store();
        s.set("k", str_val("hello"), None);
        assert_eq!(s.strlen("k").unwrap(), 5);
    }

    #[test]
    fn strlen_missing_key_returns_zero() {
        let mut s = store();
        assert_eq!(s.strlen("ghost").unwrap(), 0);
    }

    #[test]
    fn getdel_returns_and_removes() {
        let mut s = store();
        s.set("k", str_val("v"), None);
        let val = s.getdel("k").unwrap();
        assert_eq!(val, Some(Bytes::from_static(b"v")));
        assert!(s.get("k").is_none());
    }

    #[test]
    fn getdel_missing_returns_none() {
        let mut s = store();
        assert!(s.getdel("ghost").unwrap().is_none());
    }

    #[test]
    fn getset_returns_old_sets_new() {
        let mut s = store();
        s.set("k", str_val("old"), None);
        let old = s.getset("k", Bytes::from_static(b"new")).unwrap();
        assert_eq!(old, Some(Bytes::from_static(b"old")));
        match s.get("k").map(|e| &e.value) {
            Some(Value::Str(b)) => assert_eq!(b.as_ref(), b"new"),
            _ => panic!(),
        }
    }

    #[test]
    fn getset_absent_key_returns_none() {
        let mut s = store();
        let old = s.getset("k", Bytes::from_static(b"v")).unwrap();
        assert!(old.is_none());
        assert!(s.get("k").is_some());
    }

    #[test]
    fn getset_clears_ttl() {
        let mut s = store();
        s.set("k", str_val("v"), Some(Duration::from_secs(10)));
        s.getset("k", Bytes::from_static(b"new")).unwrap();
        assert_eq!(s.ttl("k"), -1);
    }

    #[test]
    fn incr_by_float_from_absent() {
        let mut s = store();
        let d = "1.5".parse::<rust_decimal::Decimal>().unwrap();
        assert_eq!(s.incr_by_float("k", d).unwrap(), "1.5");
    }

    #[test]
    fn incr_by_float_integer_result() {
        let mut s = store();
        s.set("k", Value::Str(Bytes::from_static(b"10")), None);
        let d = "5.0".parse::<rust_decimal::Decimal>().unwrap();
        assert_eq!(s.incr_by_float("k", d).unwrap(), "15");
    }

    #[test]
    fn incr_by_float_non_numeric_errors() {
        let mut s = store();
        s.set("k", str_val("abc"), None);
        let d = "1.0".parse::<rust_decimal::Decimal>().unwrap();
        assert!(matches!(s.incr_by_float("k", d), Err(Error::NotInteger)));
    }

    #[test]
    fn expire_on_existing_key_returns_true() {
        let mut s = store();
        s.set("k", str_val("v"), None);
        assert!(s.expire("k", Duration::from_secs(5)));
        assert!(s.ttl("k") > 0);
    }

    #[test]
    fn expire_on_missing_key_returns_false() {
        let mut s = store();
        assert!(!s.expire("ghost", Duration::from_secs(5)));
    }

    #[test]
    fn type_of_string() {
        let mut s = store();
        s.set("k", str_val("v"), None);
        assert_eq!(s.type_of("k"), "string");
    }

    #[test]
    fn type_of_none() {
        let mut s = store();
        assert_eq!(s.type_of("ghost"), "none");
    }

    #[test]
    fn purge_removes_expired_entries() {
        let mut s = store();
        s.set("live", str_val("v"), None);
        s.set("dead", str_val("v"), Some(Duration::from_millis(1)));
        std::thread::sleep(Duration::from_millis(5));
        s.purge_expired();
        assert!(s.get("live").is_some());
        assert!(!s.data.contains_key("dead"));
    }

    #[test]
    fn append_creates_key() {
        let mut s = store();
        assert_eq!(s.append("k", b"hello").unwrap(), 5);
    }

    #[test]
    fn append_to_existing() {
        let mut s = store();
        s.append("k", b"Hello").unwrap();
        let len = s.append("k", b", World").unwrap();
        assert_eq!(len, 12);
        match s.get("k").map(|e| &e.value) {
            Some(Value::Str(b)) => assert_eq!(b.as_ref(), b"Hello, World"),
            _ => panic!(),
        }
    }

    #[test]
    fn incr_by_absent_key_starts_at_delta() {
        let mut s = store();
        assert_eq!(s.incr_by("counter", 1).unwrap(), 1);
        assert_eq!(s.incr_by("counter", 1).unwrap(), 2);
    }

    #[test]
    fn incr_by_existing_integer() {
        let mut s = store();
        s.set("n", Value::Str(Bytes::from_static(b"10")), None);
        assert_eq!(s.incr_by("n", 5).unwrap(), 15);
    }

    #[test]
    fn incr_by_non_integer_errors() {
        let mut s = store();
        s.set("k", str_val("abc"), None);
        assert!(matches!(s.incr_by("k", 1), Err(Error::NotInteger)));
    }

    #[test]
    fn mset_then_mget() {
        let mut s = store();
        s.mset(vec![
            ("a".to_string(), Bytes::from_static(b"1")),
            ("b".to_string(), Bytes::from_static(b"2")),
        ]);
        let vals = s.mget(&["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(
            vals,
            vec![
                Some(Bytes::from_static(b"1")),
                Some(Bytes::from_static(b"2")),
                None,
            ]
        );
    }
}

/// Dual-index sorted set: O(log N) insert/remove, O(N) rank/range.
#[derive(Debug, Clone, Default)]
pub struct ZSetInner {
    pub scores: HashMap<String, f64>,
    pub by_score: BTreeMap<(OrderedFloat<f64>, String), ()>,
}

impl ZSetInner {
    /// Upsert a member. Returns `(added, score_changed)`.
    pub fn upsert(&mut self, member: String, score: f64) -> (bool, bool) {
        if let Some(&old) = self.scores.get(&member) {
            if old == score {
                return (false, false);
            }
            self.by_score.remove(&(OrderedFloat(old), member.clone()));
            self.scores.insert(member.clone(), score);
            self.by_score.insert((OrderedFloat(score), member), ());
            (false, true)
        } else {
            self.scores.insert(member.clone(), score);
            self.by_score.insert((OrderedFloat(score), member), ());
            (true, false)
        }
    }

    /// Remove a member. Returns whether it existed.
    pub fn remove(&mut self, member: &str) -> bool {
        if let Some(score) = self.scores.remove(member) {
            self.by_score
                .remove(&(OrderedFloat(score), member.to_string()));
            true
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

/// Score bound for ZCOUNT / ZRANGEBYSCORE.
#[derive(Debug, Clone)]
pub enum ScoreBound {
    NegInf,
    PosInf,
    Inclusive(f64),
    Exclusive(f64),
}

impl ScoreBound {
    fn above(&self, score: f64) -> bool {
        match self {
            ScoreBound::NegInf => true,
            ScoreBound::PosInf => false,
            ScoreBound::Inclusive(v) => score >= *v,
            ScoreBound::Exclusive(v) => score > *v,
        }
    }
    fn below(&self, score: f64) -> bool {
        match self {
            ScoreBound::NegInf => false,
            ScoreBound::PosInf => true,
            ScoreBound::Inclusive(v) => score <= *v,
            ScoreBound::Exclusive(v) => score < *v,
        }
    }
}

fn in_score_range(score: f64, min: &ScoreBound, max: &ScoreBound) -> bool {
    min.above(score) && max.below(score)
}

/// Lex bound for ZRANGEBYLEX.
#[derive(Debug, Clone)]
pub enum LexBound {
    NegInf,
    PosInf,
    Inclusive(Bytes),
    Exclusive(Bytes),
}

fn in_lex_range(member: &str, min: &LexBound, max: &LexBound) -> bool {
    let mb = member.as_bytes();
    let above = match min {
        LexBound::NegInf => true,
        LexBound::PosInf => false,
        LexBound::Inclusive(v) => mb >= v.as_ref(),
        LexBound::Exclusive(v) => mb > v.as_ref(),
    };
    let below = match max {
        LexBound::NegInf => false,
        LexBound::PosInf => true,
        LexBound::Inclusive(v) => mb <= v.as_ref(),
        LexBound::Exclusive(v) => mb < v.as_ref(),
    };
    above && below
}

/// Options for ZADD.
#[derive(Debug, Default)]
pub struct ZAddOpts {
    pub nx: bool,
    pub xx: bool,
    pub gt: bool,
    pub lt: bool,
    pub ch: bool,
    pub incr: bool,
}

#[derive(Debug, Clone)]
pub enum Value {
    Str(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<String, Bytes>),
    Set(HashSet<Bytes>),
    ZSet(ZSetInner),
}

#[derive(Debug)]
pub struct Entry {
    pub value: Value,
    pub expires_at: Option<Instant>,
}

impl Entry {
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
    }
}

/// Eviction policy applied when `maxmemory` is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvictionPolicy {
    /// Return an error on writes when at the memory limit (default).
    #[default]
    NoEviction,
    /// Evict any key using approximate LRU.
    AllKeysLru,
    /// Evict only keys with a TTL using approximate LRU.
    VolatileLru,
    /// Evict any key at random.
    AllKeysRandom,
    /// Evict a random key that has a TTL.
    VolatileRandom,
    /// Evict the key with the shortest remaining TTL.
    VolatileTtl,
}

impl EvictionPolicy {
    pub fn parse(s: &str) -> Self {
        match s {
            "allkeys-lru" => Self::AllKeysLru,
            "volatile-lru" => Self::VolatileLru,
            "allkeys-random" => Self::AllKeysRandom,
            "volatile-random" => Self::VolatileRandom,
            "volatile-ttl" => Self::VolatileTtl,
            _ => Self::NoEviction,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoEviction => "noeviction",
            Self::AllKeysLru => "allkeys-lru",
            Self::VolatileLru => "volatile-lru",
            Self::AllKeysRandom => "allkeys-random",
            Self::VolatileRandom => "volatile-random",
            Self::VolatileTtl => "volatile-ttl",
        }
    }
}

pub struct Store {
    data: HashMap<String, Entry>,
    /// Bytes limit (0 = disabled).
    maxmemory: u64,
    eviction_policy: EvictionPolicy,
    /// Per-key LRU clock — monotonically increasing tick recorded on each access.
    /// Stored separately to avoid touching Entry struct literals.
    lru_clock: HashMap<String, u64>,
    lru_tick: u64,
}

impl Store {
    pub fn new() -> Self {
        Store {
            data: HashMap::new(),
            maxmemory: 0,
            eviction_policy: EvictionPolicy::NoEviction,
            lru_clock: HashMap::new(),
            lru_tick: 0,
        }
    }

    /// Apply maxmemory and eviction policy from config. Called once at startup.
    pub fn configure_memory(&mut self, maxmemory: u64, policy: &str) {
        self.maxmemory = maxmemory;
        self.eviction_policy = EvictionPolicy::parse(policy);
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    /// Get a non-expired entry. Removes the key lazily if expired.
    /// Updates the LRU clock on each successful access.
    pub fn get(&mut self, key: &str) -> Option<&Entry> {
        if self.data.get(key).is_some_and(|entry| entry.is_expired()) {
            self.data.remove(key);
            self.lru_clock.remove(key);
            return None;
        }
        if self.data.contains_key(key) {
            let tick = self.next_lru_tick();
            self.lru_clock.insert(key.to_string(), tick);
        }
        self.data.get(key)
    }

    pub fn set(&mut self, key: &str, value: Value, ttl: Option<Duration>) {
        let tick = self.next_lru_tick();
        self.lru_clock.insert(key.to_string(), tick);
        let expires_at = ttl.map(|duration| Instant::now() + duration);
        self.data
            .insert(key.to_string(), Entry { value, expires_at });
    }

    /// Return the number of actual keys deleted.
    pub fn del(&mut self, keys: &[String]) -> u64 {
        let mut count = 0u64;
        for k in keys {
            if self.data.remove(k.as_str()).is_some() {
                self.lru_clock.remove(k.as_str());
                count += 1;
            }
        }
        count
    }

    /// Counts existing non-expired keys. A repeated key counts multiple times.
    pub fn exists(&mut self, key: &[String]) -> u64 {
        key.iter()
            .filter(|k| self.get(k.as_str()).is_some())
            .count() as u64
    }

    /// Set TTL on an existing key. Return false if the key is missing or expired.
    pub fn expire(&mut self, key: &str, ttl: Duration) -> bool {
        if self.get(key).is_none() {
            return false;
        }
        if let Some(entry) = self.data.get_mut(key) {
            entry.expires_at = Some(Instant::now() + ttl);
            return true;
        }
        false
    }

    /// -2 = missing/expired, -1 = no expiry, N = seconds remaining
    pub fn ttl(&mut self, key: &str) -> i64 {
        match self.get(key) {
            None => -2,
            Some(entry) => match entry.expires_at {
                None => -1,
                Some(t) => {
                    let remaining = t.saturating_duration_since(Instant::now());
                    (remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)) as i64
                }
            },
        }
    }

    /// Remove the TTL from a key. Returns false if key missing/expired or had no TTL.
    pub fn persist(&mut self, key: &str) -> bool {
        if self.get(key).is_none() {
            return false;
        }
        if let Some(entry) = self.data.get_mut(key)
            && entry.expires_at.take().is_some()
        {
            return true;
        }
        false
    }

    /// -2 = missing/expired, -1 = no expiry, N = milliseconds remaining (ceiling)
    pub fn pttl(&mut self, key: &str) -> i64 {
        match self.get(key) {
            None => -2,
            Some(entry) => match entry.expires_at {
                None => -1,
                Some(t) => {
                    let remaining = t.saturating_duration_since(Instant::now());
                    remaining.as_nanos().div_ceil(1_000_000) as i64
                }
            },
        }
    }

    /// Returns the Redis type name for the key, or "none" if missing/expired.
    pub fn type_of(&mut self, key: &str) -> &'static str {
        match self.get(key) {
            None => "none",
            Some(entry) => match entry.value {
                Value::Str(_) => "string",
                Value::Hash(_) => "hash",
                Value::List(_) => "list",
                Value::Set(_) => "set",
                Value::ZSet(_) => "zset",
            },
        }
    }

    /// Background sweeper: remove all expired entries.
    pub fn purge_expired(&mut self) {
        self.data.retain(|_, v| !v.is_expired());
        // Drop lru_clock entries for keys that were just evicted.
        let data = &self.data;
        self.lru_clock.retain(|k, _| data.contains_key(k.as_str()));
    }

    /// Returns all non-expired keys matching a glob pattern (`*`, `?` wildcards).
    pub fn keys(&mut self, pattern: &str) -> Vec<String> {
        self.purge_expired();
        let p: Vec<char> = pattern.chars().collect();
        self.data
            .keys()
            .filter(|k| glob_match(&p, &k.chars().collect::<Vec<_>>()))
            .cloned()
            .collect()
    }

    /// Number of non-expired keys.
    pub fn dbsize(&mut self) -> usize {
        self.purge_expired();
        self.data.len()
    }

    /// Rename `key` to `newkey`, preserving value, TTL, and LRU clock.
    /// Returns `Err` if `key` does not exist.
    pub fn rename(&mut self, key: &str, newkey: &str) -> Result<()> {
        if self.get(key).is_none() {
            return Err(Error::Protocol("no such key".into()));
        }
        if let Some(entry) = self.data.remove(key) {
            self.data.remove(newkey);
            let tick = self.lru_clock.remove(key).unwrap_or(0);
            self.lru_clock.remove(newkey);
            self.data.insert(newkey.to_string(), entry);
            self.lru_clock.insert(newkey.to_string(), tick);
        }
        Ok(())
    }

    /// Remove all keys.
    pub fn flushdb(&mut self) {
        self.data.clear();
        self.lru_clock.clear();
    }

    /// Returns (total_keys, keys_with_expiry) — purges expired entries first.
    pub fn keyspace_info(&mut self) -> (usize, usize) {
        self.purge_expired();
        let total = self.data.len();
        let with_expiry = self
            .data
            .values()
            .filter(|e| e.expires_at.is_some())
            .count();
        (total, with_expiry)
    }

    /// Direct inserts
    pub fn restore(&mut self, key: String, entry: Entry) {
        self.data.insert(key, entry);
    }

    /// Append bytes to a string values (creates key if absent)
    /// Returns the new length in bytes
    pub fn append(&mut self, key: &str, extra: &[u8]) -> Result<u64> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Str(b) => {
                    let mut v = b.to_vec();
                    v.extend_from_slice(extra);
                    let len = v.len() as u64;
                    *b = Bytes::from(v);
                    Ok(len)
                }
                _ => Err(Error::WrongType),
            },
            _ => {
                let data = Bytes::copy_from_slice(extra);
                let len = data.len() as u64;
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Str(data),
                        expires_at: None,
                    },
                );

                Ok(len)
            }
        }
    }
    /// Increment key by `delta`. Key must be absent or hold a decimal integer string.
    pub fn incr_by(&mut self, key: &str, delta: i64) -> Result<i64> {
        match self.get(key) {
            None => {
                let result = delta;
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Str(Bytes::from(result.to_string())),
                        expires_at: None,
                    },
                );
                Ok(result)
            }
            Some(entry) => match &entry.value {
                Value::Str(b) => {
                    let s = std::str::from_utf8(b).map_err(|_| Error::NotInteger)?;
                    let n: i64 = s.parse().map_err(|_| Error::NotInteger)?;
                    let result = n.checked_add(delta).ok_or(Error::NotInteger)?;
                    // re-acquire mut ref
                    if let Some(e) = self.data.get_mut(key) {
                        e.value = Value::Str(Bytes::from(result.to_string()));
                    }
                    Ok(result)
                }
                _ => Err(Error::WrongType),
            },
        }
    }

    /// Returns one `Option<Bytes>` per key (None for missing/expired keys).
    pub fn mget(&mut self, keys: &[String]) -> Vec<Option<Bytes>> {
        keys.iter()
            .map(|k| {
                self.get(k).and_then(|e| match &e.value {
                    Value::Str(b) => Some(b.clone()),
                    _ => None, // Redis returns nil for wrong-type keys in MGET
                })
            })
            .collect()
    }

    /// Set multiple key/value pairs atomically.
    pub fn mset(&mut self, pairs: Vec<(String, Bytes)>) {
        for (k, v) in pairs {
            self.set(&k, Value::Str(v), None);
        }
    }

    /// Length of the string value in bytes. Returns 0 for missing keys.
    pub fn strlen(&mut self, key: &str) -> Result<u64> {
        match self.get(key) {
            None => Ok(0),
            Some(entry) => match &entry.value {
                Value::Str(b) => Ok(b.len() as u64),
                _ => Err(Error::WrongType),
            },
        }
    }

    /// Get the value and atomically delete the key.
    pub fn getdel(&mut self, key: &str) -> Result<Option<Bytes>> {
        match self.get(key) {
            None => return Ok(None),
            Some(entry) if !matches!(entry.value, Value::Str(_)) => return Err(Error::WrongType),
            _ => {}
        }
        Ok(self.data.remove(key).map(|e| match e.value {
            Value::Str(b) => b,
            _ => unreachable!(),
        }))
    }

    /// Set a new value and return the previous one (nil if key was absent).
    /// Clears any existing TTL on the key.
    pub fn getset(&mut self, key: &str, new_value: Bytes) -> Result<Option<Bytes>> {
        match self.get(key) {
            Some(entry) if !matches!(entry.value, Value::Str(_)) => return Err(Error::WrongType),
            _ => {}
        }
        let old = self.data.remove(key).and_then(|e| {
            if e.is_expired() {
                return None;
            }
            match e.value {
                Value::Str(b) => Some(b),
                _ => unreachable!(),
            }
        });
        self.data.insert(
            key.to_string(),
            Entry {
                value: Value::Str(new_value),
                expires_at: None,
            },
        );
        Ok(old)
    }

    /// Increment key by a decimal delta. Value must be a valid numeric string.
    /// Returns the new value formatted as a string (exact decimal arithmetic).
    pub fn incr_by_float(&mut self, key: &str, delta: rust_decimal::Decimal) -> Result<String> {
        use rust_decimal::Decimal;
        let current = match self.get(key) {
            None => Decimal::ZERO,
            Some(entry) => match &entry.value {
                Value::Str(b) => std::str::from_utf8(b)
                    .ok()
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .ok_or(Error::NotInteger)?,
                _ => return Err(Error::WrongType),
            },
        };
        let result = current + delta;
        let result_str = format_decimal(result);
        let new_val = Bytes::from(result_str.clone());
        match self.data.get_mut(key) {
            Some(entry) => entry.value = Value::Str(new_val),
            None => {
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Str(new_val),
                        expires_at: None,
                    },
                );
            }
        }
        Ok(result_str)
    }

    pub fn hset(&mut self, key: &str, pairs: Vec<(String, Bytes)>) -> Result<u64> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Hash(map) => {
                    let mut added = 0u64;
                    for (k, v) in pairs {
                        if map.insert(k, v).is_none() {
                            added += 1;
                        }
                    }
                    Ok(added)
                }
                _ => Err(Error::WrongType),
            },
            _ => {
                let mut map = HashMap::new();
                let added = pairs.len() as u64;
                for (k, v) in pairs {
                    map.insert(k, v);
                }
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Hash(map),
                        expires_at: None,
                    },
                );
                Ok(added)
            }
        }
    }

    pub fn hget(&mut self, key: &str, field: &str) -> Result<Option<Bytes>> {
        match self.get(key) {
            None => Ok(None),
            Some(entry) => match &entry.value {
                Value::Hash(map) => Ok(map.get(field).cloned()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn hdel(&mut self, key: &str, fields: &[String]) -> Result<u64> {
        let (removed, empty) = match self.data.get_mut(key) {
            None => return Ok(0),
            Some(entry) if entry.is_expired() => return Ok(0),
            Some(entry) => match &mut entry.value {
                Value::Hash(map) => {
                    let n = fields
                        .iter()
                        .filter(|f| map.remove(f.as_str()).is_some())
                        .count() as u64;
                    (n, map.is_empty())
                }
                _ => return Err(Error::WrongType),
            },
        };
        if empty {
            self.data.remove(key);
        }
        Ok(removed)
    }

    pub fn hexists(&mut self, key: &str, field: &str) -> Result<bool> {
        match self.get(key) {
            None => Ok(false),
            Some(entry) => match &entry.value {
                Value::Hash(map) => Ok(map.contains_key(field)),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn hlen(&mut self, key: &str) -> Result<u64> {
        match self.get(key) {
            None => Ok(0),
            Some(entry) => match &entry.value {
                Value::Hash(map) => Ok(map.len() as u64),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn hkeys(&mut self, key: &str) -> Result<Vec<String>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::Hash(map) => Ok(map.keys().cloned().collect()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn hvals(&mut self, key: &str) -> Result<Vec<Bytes>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::Hash(map) => Ok(map.values().cloned().collect()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn hgetall(&mut self, key: &str) -> Result<Vec<(String, Bytes)>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::Hash(map) => Ok(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn hmget(&mut self, key: &str, fields: &[String]) -> Result<Vec<Option<Bytes>>> {
        match self.get(key) {
            None => Ok(fields.iter().map(|_| None).collect()),
            Some(entry) => match &entry.value {
                Value::Hash(map) => Ok(fields
                    .iter()
                    .map(|f| map.get(f.as_str()).cloned())
                    .collect()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn hincrby(&mut self, key: &str, field: &str, delta: i64) -> Result<i64> {
        let current: i64 = match self.get(key) {
            None => 0,
            Some(entry) => match &entry.value {
                Value::Hash(map) => match map.get(field) {
                    None => 0,
                    Some(b) => std::str::from_utf8(b)
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .ok_or(Error::NotInteger)?,
                },
                _ => return Err(Error::WrongType),
            },
        };
        let result = current.checked_add(delta).ok_or(Error::NotInteger)?;
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Hash(map) => {
                    map.insert(field.to_string(), Bytes::from(result.to_string()));
                }
                _ => unreachable!(),
            },
            _ => {
                let mut map = HashMap::new();
                map.insert(field.to_string(), Bytes::from(result.to_string()));
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Hash(map),
                        expires_at: None,
                    },
                );
            }
        }
        Ok(result)
    }

    pub fn hincrbyfloat(
        &mut self,
        key: &str,
        field: &str,
        delta: rust_decimal::Decimal,
    ) -> Result<String> {
        use rust_decimal::Decimal;
        let current: Decimal = match self.get(key) {
            None => Decimal::ZERO,
            Some(entry) => match &entry.value {
                Value::Hash(map) => match map.get(field) {
                    None => Decimal::ZERO,
                    Some(b) => std::str::from_utf8(b)
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .ok_or(Error::NotInteger)?,
                },
                _ => return Err(Error::WrongType),
            },
        };
        let result_str = format_decimal(current + delta);
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Hash(map) => {
                    map.insert(field.to_string(), Bytes::from(result_str.clone()));
                }
                _ => unreachable!(),
            },
            _ => {
                let mut map = HashMap::new();
                map.insert(field.to_string(), Bytes::from(result_str.clone()));
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Hash(map),
                        expires_at: None,
                    },
                );
            }
        }
        Ok(result_str)
    }

    pub fn hsetnx(&mut self, key: &str, field: &str, value: Bytes) -> Result<bool> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Hash(map) => {
                    if map.contains_key(field) {
                        return Ok(false);
                    }
                    map.insert(field.to_string(), value);
                    Ok(true)
                }
                _ => Err(Error::WrongType),
            },
            _ => {
                let mut map = HashMap::new();
                map.insert(field.to_string(), value);
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Hash(map),
                        expires_at: None,
                    },
                );
                Ok(true)
            }
        }
    }

    // ── List methods ─────────────────────────────────────────────────────────

    pub fn lpush(&mut self, key: &str, values: &[Bytes]) -> Result<usize> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => {
                    for v in values {
                        deq.push_front(v.clone());
                    }
                    Ok(deq.len())
                }
                _ => Err(Error::WrongType),
            },
            _ => {
                let mut deq = VecDeque::new();
                for v in values {
                    deq.push_front(v.clone());
                }
                let len = deq.len();
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::List(deq),
                        expires_at: None,
                    },
                );
                Ok(len)
            }
        }
    }

    pub fn rpush(&mut self, key: &str, values: &[Bytes]) -> Result<usize> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => {
                    for v in values {
                        deq.push_back(v.clone());
                    }
                    Ok(deq.len())
                }
                _ => Err(Error::WrongType),
            },
            _ => {
                let mut deq = VecDeque::new();
                for v in values {
                    deq.push_back(v.clone());
                }
                let len = deq.len();
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::List(deq),
                        expires_at: None,
                    },
                );
                Ok(len)
            }
        }
    }

    pub fn lpop(&mut self, key: &str, count: usize) -> Result<Option<Vec<Bytes>>> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => {
                    if deq.is_empty() {
                        (Ok(None), true)
                    } else {
                        let n = count.min(deq.len());
                        let result: Vec<Bytes> = (0..n).filter_map(|_| deq.pop_front()).collect();
                        let empty = deq.is_empty();
                        (Ok(Some(result)), empty)
                    }
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(None), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    pub fn rpop(&mut self, key: &str, count: usize) -> Result<Option<Vec<Bytes>>> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => {
                    if deq.is_empty() {
                        (Ok(None), true)
                    } else {
                        let n = count.min(deq.len());
                        let result: Vec<Bytes> = (0..n).filter_map(|_| deq.pop_back()).collect();
                        let empty = deq.is_empty();
                        (Ok(Some(result)), empty)
                    }
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(None), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    pub fn llen(&mut self, key: &str) -> Result<usize> {
        match self.get(key) {
            None => Ok(0),
            Some(entry) => match &entry.value {
                Value::List(deq) => Ok(deq.len()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn lrange(&mut self, key: &str, start: i64, stop: i64) -> Result<Vec<Bytes>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::List(deq) => Ok(match resolve_range(start, stop, deq.len()) {
                    None => vec![],
                    Some((s, e)) => deq.range(s..=e).cloned().collect(),
                }),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn lindex(&mut self, key: &str, index: i64) -> Result<Option<Bytes>> {
        match self.get(key) {
            None => Ok(None),
            Some(entry) => match &entry.value {
                Value::List(deq) => {
                    Ok(normalize_index(index, deq.len()).and_then(|i| deq.get(i).cloned()))
                }
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn lset(&mut self, key: &str, index: i64, value: Bytes) -> Result<()> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => {
                    let i = normalize_index(index, deq.len())
                        .ok_or(Error::Protocol("index out of range".into()))?;
                    deq[i] = value;
                    Ok(())
                }
                _ => Err(Error::WrongType),
            },
            _ => Err(Error::Protocol("no such key".into())),
        }
    }

    /// Returns: 0 = key not found, -1 = pivot not found, else new length.
    pub fn linsert(
        &mut self,
        key: &str,
        before: bool,
        pivot: &Bytes,
        element: Bytes,
    ) -> Result<i64> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => match deq.iter().position(|x| x == pivot) {
                    None => Ok(-1),
                    Some(pos) => {
                        let insert_at = if before { pos } else { pos + 1 };
                        deq.insert(insert_at, element);
                        Ok(deq.len() as i64)
                    }
                },
                _ => Err(Error::WrongType),
            },
            _ => Ok(0),
        }
    }

    pub fn lrem(&mut self, key: &str, count: i64, element: &Bytes) -> Result<usize> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => {
                    let before = deq.len();
                    let max = count.unsigned_abs() as usize;
                    if count == 0 {
                        deq.retain(|x| x != element);
                    } else if count > 0 {
                        let mut removed = 0usize;
                        deq.retain(|x| {
                            if removed < max && x == element {
                                removed += 1;
                                false
                            } else {
                                true
                            }
                        });
                    } else {
                        // remove from tail: reverse, remove from front, reverse back
                        let mut rev: VecDeque<Bytes> = deq.iter().cloned().rev().collect();
                        let mut removed = 0usize;
                        rev.retain(|x| {
                            if removed < max && x == element {
                                removed += 1;
                                false
                            } else {
                                true
                            }
                        });
                        *deq = rev.into_iter().rev().collect();
                    }
                    let removed = before - deq.len();
                    (Ok(removed), deq.is_empty())
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(0), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    pub fn ltrim(&mut self, key: &str, start: i64, stop: i64) -> Result<()> {
        let became_empty = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => match resolve_range(start, stop, deq.len()) {
                    None => {
                        deq.clear();
                        true
                    }
                    Some((s, e)) => {
                        deq.drain(0..s);
                        let new_len = e - s + 1;
                        deq.truncate(new_len);
                        deq.is_empty()
                    }
                },
                _ => return Err(Error::WrongType),
            },
            _ => false,
        };
        if became_empty {
            self.data.remove(key);
        }
        Ok(())
    }

    pub fn lmove(
        &mut self,
        src: &str,
        dst: &str,
        wherefrom: ListDirection,
        whereto: ListDirection,
    ) -> Result<Option<Bytes>> {
        // Phase 1: pop element from src
        let element = {
            let entry = match self.data.get_mut(src) {
                Some(e) if !e.is_expired() => e,
                _ => return Ok(None),
            };
            match &mut entry.value {
                Value::List(deq) => match wherefrom {
                    ListDirection::Left => deq.pop_front(),
                    ListDirection::Right => deq.pop_back(),
                },
                _ => return Err(Error::WrongType),
            }
        };

        let element = match element {
            None => return Ok(None),
            Some(e) => e,
        };

        // Phase 2: auto-delete src if empty
        let src_empty = self
            .data
            .get(src)
            .is_some_and(|e| matches!(&e.value, Value::List(d) if d.is_empty()));
        if src_empty {
            self.data.remove(src);
        }

        // Phase 3: push element to dst
        match self.data.get_mut(dst) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::List(deq) => match whereto {
                    ListDirection::Left => deq.push_front(element.clone()),
                    ListDirection::Right => deq.push_back(element.clone()),
                },
                _ => return Err(Error::WrongType),
            },
            _ => {
                let mut deq = VecDeque::new();
                match whereto {
                    ListDirection::Left => deq.push_front(element.clone()),
                    ListDirection::Right => deq.push_back(element.clone()),
                }
                self.data.insert(
                    dst.to_string(),
                    Entry {
                        value: Value::List(deq),
                        expires_at: None,
                    },
                );
            }
        }

        Ok(Some(element))
    }

    // ── Set methods ──────────────────────────────────────────────────────────

    pub fn sadd(&mut self, key: &str, members: &[Bytes]) -> Result<usize> {
        match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Set(s) => {
                    let before = s.len();
                    s.extend(members.iter().cloned());
                    Ok(s.len() - before)
                }
                _ => Err(Error::WrongType),
            },
            _ => {
                let mut s: HashSet<Bytes> = HashSet::new();
                s.extend(members.iter().cloned());
                let added = s.len();
                self.data.insert(
                    key.to_string(),
                    Entry {
                        value: Value::Set(s),
                        expires_at: None,
                    },
                );
                Ok(added)
            }
        }
    }

    pub fn srem(&mut self, key: &str, members: &[Bytes]) -> Result<usize> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Set(s) => {
                    let removed = members.iter().filter(|m| s.remove(*m)).count();
                    (Ok(removed), s.is_empty())
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(0), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    pub fn sismember(&mut self, key: &str, member: &Bytes) -> Result<bool> {
        match self.get(key) {
            None => Ok(false),
            Some(entry) => match &entry.value {
                Value::Set(s) => Ok(s.contains(member)),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn smismember(&mut self, key: &str, members: &[Bytes]) -> Result<Vec<bool>> {
        match self.get(key) {
            None => Ok(members.iter().map(|_| false).collect()),
            Some(entry) => match &entry.value {
                Value::Set(s) => Ok(members.iter().map(|m| s.contains(m)).collect()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn smembers(&mut self, key: &str) -> Result<Vec<Bytes>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::Set(s) => Ok(s.iter().cloned().collect()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn scard(&mut self, key: &str) -> Result<usize> {
        match self.get(key) {
            None => Ok(0),
            Some(entry) => match &entry.value {
                Value::Set(s) => Ok(s.len()),
                _ => Err(Error::WrongType),
            },
        }
    }

    /// Positive count: up to `count` distinct members.
    /// Negative count: `|count|` members, possibly repeated (cycling iterator).
    pub fn srandmember(&mut self, key: &str, count: i64) -> Result<Vec<Bytes>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::Set(s) => {
                    if count >= 0 {
                        let n = (count as usize).min(s.len());
                        Ok(s.iter().take(n).cloned().collect())
                    } else {
                        let n = count.unsigned_abs() as usize;
                        let members: Vec<&Bytes> = s.iter().collect();
                        Ok((0..n).map(|i| members[i % members.len()].clone()).collect())
                    }
                }
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn spop(&mut self, key: &str, count: usize) -> Result<Vec<Bytes>> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Set(s) => {
                    let n = count.min(s.len());
                    let popped: Vec<Bytes> = s.iter().take(n).cloned().collect();
                    for m in &popped {
                        s.remove(m);
                    }
                    let empty = s.is_empty();
                    (Ok(popped), empty)
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(vec![]), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    fn get_set_clone(&mut self, key: &str) -> Result<HashSet<Bytes>> {
        match self.get(key) {
            None => Ok(HashSet::new()),
            Some(entry) => match &entry.value {
                Value::Set(s) => Ok(s.clone()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn sunion(&mut self, keys: &[String]) -> Result<Vec<Bytes>> {
        let mut result: HashSet<Bytes> = HashSet::new();
        for key in keys {
            result.extend(self.get_set_clone(key)?);
        }
        Ok(result.into_iter().collect())
    }

    pub fn sinter(&mut self, keys: &[String]) -> Result<Vec<Bytes>> {
        if keys.is_empty() {
            return Ok(vec![]);
        }
        let mut sets: Vec<HashSet<Bytes>> = Vec::with_capacity(keys.len());
        for key in keys {
            sets.push(self.get_set_clone(key)?);
        }
        let (first, rest) = sets.split_first().unwrap();
        Ok(first
            .iter()
            .filter(|m| rest.iter().all(|s| s.contains(*m)))
            .cloned()
            .collect())
    }

    pub fn sdiff(&mut self, keys: &[String]) -> Result<Vec<Bytes>> {
        if keys.is_empty() {
            return Ok(vec![]);
        }
        let mut sets: Vec<HashSet<Bytes>> = Vec::with_capacity(keys.len());
        for key in keys {
            sets.push(self.get_set_clone(key)?);
        }
        let (first, rest) = sets.split_first().unwrap();
        Ok(first
            .iter()
            .filter(|m| !rest.iter().any(|s| s.contains(*m)))
            .cloned()
            .collect())
    }

    pub fn sunionstore(&mut self, dest: &str, keys: &[String]) -> Result<usize> {
        let result = self.sunion(keys)?;
        let len = result.len();
        if len == 0 {
            self.data.remove(dest);
        } else {
            let s: HashSet<Bytes> = result.into_iter().collect();
            self.data.insert(
                dest.to_string(),
                Entry {
                    value: Value::Set(s),
                    expires_at: None,
                },
            );
        }
        Ok(len)
    }

    pub fn sinterstore(&mut self, dest: &str, keys: &[String]) -> Result<usize> {
        let result = self.sinter(keys)?;
        let len = result.len();
        if len == 0 {
            self.data.remove(dest);
        } else {
            let s: HashSet<Bytes> = result.into_iter().collect();
            self.data.insert(
                dest.to_string(),
                Entry {
                    value: Value::Set(s),
                    expires_at: None,
                },
            );
        }
        Ok(len)
    }

    pub fn sdiffstore(&mut self, dest: &str, keys: &[String]) -> Result<usize> {
        let result = self.sdiff(keys)?;
        let len = result.len();
        if len == 0 {
            self.data.remove(dest);
        } else {
            let s: HashSet<Bytes> = result.into_iter().collect();
            self.data.insert(
                dest.to_string(),
                Entry {
                    value: Value::Set(s),
                    expires_at: None,
                },
            );
        }
        Ok(len)
    }

    /// Returns true if the member was moved.
    pub fn smove(&mut self, src: &str, dst: &str, member: &Bytes) -> Result<bool> {
        // Phase 1: remove from src
        let found = {
            let entry = match self.data.get_mut(src) {
                Some(e) if !e.is_expired() => e,
                _ => return Ok(false),
            };
            match &mut entry.value {
                Value::Set(s) => s.remove(member),
                _ => return Err(Error::WrongType),
            }
        };
        if !found {
            return Ok(false);
        }
        // Phase 2: auto-delete src if empty
        let src_empty = self
            .data
            .get(src)
            .is_some_and(|e| matches!(&e.value, Value::Set(s) if s.is_empty()));
        if src_empty {
            self.data.remove(src);
        }
        // Phase 3: check dst type, then add
        match self.data.get_mut(dst) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::Set(s) => {
                    s.insert(member.clone());
                }
                _ => return Err(Error::WrongType),
            },
            _ => {
                let mut s = HashSet::new();
                s.insert(member.clone());
                self.data.insert(
                    dst.to_string(),
                    Entry {
                        value: Value::Set(s),
                        expires_at: None,
                    },
                );
            }
        }
        Ok(true)
    }

    // ── ZSet methods ─────────────────────────────────────────────────────────

    pub fn zadd(&mut self, key: &str, entries: &[(f64, String)], opts: &ZAddOpts) -> Result<i64> {
        // Get or create the ZSet
        match self.data.get(key) {
            Some(e) if !e.is_expired() && !matches!(e.value, Value::ZSet(_)) => {
                return Err(Error::WrongType);
            }
            _ => {}
        }
        if !self.data.contains_key(key) || self.data.get(key).is_some_and(|e| e.is_expired()) {
            self.data.insert(
                key.to_string(),
                Entry {
                    value: Value::ZSet(ZSetInner::default()),
                    expires_at: None,
                },
            );
        }
        let zset = match &mut self.data.get_mut(key).unwrap().value {
            Value::ZSet(z) => z,
            _ => unreachable!(),
        };

        let mut added = 0i64;
        let mut changed = 0i64;
        for (score, member) in entries {
            let current = zset.scores.get(member.as_str()).copied();
            let should_update = match (opts.nx, opts.xx, opts.gt, opts.lt, current) {
                (true, _, _, _, Some(_)) => false,          // NX: skip existing
                (_, true, _, _, None) => false,             // XX: skip new
                (_, _, true, _, Some(cur)) => *score > cur, // GT
                (_, _, _, true, Some(cur)) => *score < cur, // LT
                _ => true,
            };
            if should_update {
                let (add, change) = zset.upsert(member.clone(), *score);
                if add {
                    added += 1;
                }
                if add || change {
                    changed += 1;
                }
            }
        }
        Ok(if opts.ch { changed } else { added })
    }

    pub fn zrem(&mut self, key: &str, members: &[String]) -> Result<usize> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::ZSet(z) => {
                    let removed = members.iter().filter(|m| z.remove(m)).count();
                    (Ok(removed), z.is_empty())
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(0), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    pub fn zremrangebyrank(&mut self, key: &str, start: i64, stop: i64) -> Result<usize> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::ZSet(z) => {
                    let all: Vec<String> = z.by_score.keys().map(|(_, m)| m.clone()).collect();
                    let to_remove = resolve_range(start, stop, all.len())
                        .map(|(s, e)| all[s..=e].to_vec())
                        .unwrap_or_default();
                    let removed = to_remove.iter().filter(|m| z.remove(m)).count();
                    (Ok(removed), z.is_empty())
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(0), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    pub fn zscore(&mut self, key: &str, member: &str) -> Result<Option<f64>> {
        match self.get(key) {
            None => Ok(None),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => Ok(z.scores.get(member).copied()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn zincrby(&mut self, key: &str, member: &str, delta: f64) -> Result<f64> {
        match self.data.get(key) {
            Some(e) if !e.is_expired() && !matches!(e.value, Value::ZSet(_)) => {
                return Err(Error::WrongType);
            }
            _ => {}
        }
        if !self.data.contains_key(key) || self.data.get(key).is_some_and(|e| e.is_expired()) {
            self.data.insert(
                key.to_string(),
                Entry {
                    value: Value::ZSet(ZSetInner::default()),
                    expires_at: None,
                },
            );
        }
        let zset = match &mut self.data.get_mut(key).unwrap().value {
            Value::ZSet(z) => z,
            _ => unreachable!(),
        };
        let new_score = zset.scores.get(member).copied().unwrap_or(0.0) + delta;
        zset.upsert(member.to_string(), new_score);
        Ok(new_score)
    }

    pub fn zrank(&mut self, key: &str, member: &str) -> Result<Option<usize>> {
        match self.get(key) {
            None => Ok(None),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => Ok(z.scores.get(member).map(|&score| {
                    z.by_score
                        .range(..(OrderedFloat(score), member.to_string()))
                        .count()
                })),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn zrevrank(&mut self, key: &str, member: &str) -> Result<Option<usize>> {
        match self.get(key) {
            None => Ok(None),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => Ok(z.scores.get(member).map(|&score| {
                    let key = (OrderedFloat(score), member.to_string());
                    z.by_score
                        .range((std::ops::Bound::Excluded(key), std::ops::Bound::Unbounded))
                        .count()
                })),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn zcard(&mut self, key: &str) -> Result<usize> {
        match self.get(key) {
            None => Ok(0),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => Ok(z.len()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn zcount(&mut self, key: &str, min: &ScoreBound, max: &ScoreBound) -> Result<usize> {
        match self.get(key) {
            None => Ok(0),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => Ok(z
                    .by_score
                    .keys()
                    .filter(|(s, _)| in_score_range(s.0, min, max))
                    .count()),
                _ => Err(Error::WrongType),
            },
        }
    }

    /// Range by index. Returns (member, score) pairs in ascending order (or descending if rev).
    pub fn zrange_by_index(
        &mut self,
        key: &str,
        start: i64,
        stop: i64,
        rev: bool,
    ) -> Result<Vec<(String, f64)>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => {
                    let all: Vec<(String, f64)> =
                        z.by_score.keys().map(|(s, m)| (m.clone(), s.0)).collect();
                    let len = all.len();
                    match resolve_range(start, stop, len) {
                        None => Ok(vec![]),
                        Some((s, e)) => {
                            let mut slice: Vec<(String, f64)> = all[s..=e].to_vec();
                            if rev {
                                slice.reverse();
                            }
                            Ok(slice)
                        }
                    }
                }
                _ => Err(Error::WrongType),
            },
        }
    }

    /// Range by score. Returns (member, score) pairs.
    pub fn zrange_by_score(
        &mut self,
        key: &str,
        min: &ScoreBound,
        max: &ScoreBound,
        rev: bool,
        limit: Option<(usize, usize)>,
    ) -> Result<Vec<(String, f64)>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => {
                    let mut results: Vec<(String, f64)> = z
                        .by_score
                        .keys()
                        .filter(|(s, _)| in_score_range(s.0, min, max))
                        .map(|(s, m)| (m.clone(), s.0))
                        .collect();
                    if rev {
                        results.reverse();
                    }
                    if let Some((offset, count)) = limit {
                        results = results.into_iter().skip(offset).take(count).collect();
                    }
                    Ok(results)
                }
                _ => Err(Error::WrongType),
            },
        }
    }

    /// Range by lex (all members must share the same score). Returns member names.
    pub fn zrange_by_lex(
        &mut self,
        key: &str,
        min: &LexBound,
        max: &LexBound,
        rev: bool,
        limit: Option<(usize, usize)>,
    ) -> Result<Vec<String>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => {
                    let mut results: Vec<String> = z
                        .by_score
                        .keys()
                        .filter(|(_, m)| in_lex_range(m, min, max))
                        .map(|(_, m)| m.clone())
                        .collect();
                    if rev {
                        results.reverse();
                    }
                    if let Some((offset, count)) = limit {
                        results = results.into_iter().skip(offset).take(count).collect();
                    }
                    Ok(results)
                }
                _ => Err(Error::WrongType),
            },
        }
    }

    /// Pop `count` members with the lowest scores.
    pub fn zpopmin(&mut self, key: &str, count: usize) -> Result<Vec<(String, f64)>> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::ZSet(z) => {
                    let n = count.min(z.len());
                    let to_pop: Vec<(String, f64)> = z
                        .by_score
                        .keys()
                        .take(n)
                        .map(|(s, m)| (m.clone(), s.0))
                        .collect();
                    for (m, _) in &to_pop {
                        z.remove(m);
                    }
                    let empty = z.is_empty();
                    (Ok(to_pop), empty)
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(vec![]), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    /// Pop `count` members with the highest scores.
    pub fn zpopmax(&mut self, key: &str, count: usize) -> Result<Vec<(String, f64)>> {
        let (ret, became_empty) = match self.data.get_mut(key) {
            Some(entry) if !entry.is_expired() => match &mut entry.value {
                Value::ZSet(z) => {
                    let n = count.min(z.len());
                    let to_pop: Vec<(String, f64)> = z
                        .by_score
                        .keys()
                        .rev()
                        .take(n)
                        .map(|(s, m)| (m.clone(), s.0))
                        .collect();
                    for (m, _) in &to_pop {
                        z.remove(m);
                    }
                    let empty = z.is_empty();
                    (Ok(to_pop), empty)
                }
                _ => (Err(Error::WrongType), false),
            },
            _ => (Ok(vec![]), false),
        };
        if became_empty {
            self.data.remove(key);
        }
        ret
    }

    /// Returns random members. Positive count: distinct. Negative: with repeats.
    pub fn zrandmember(&mut self, key: &str, count: i64) -> Result<Vec<String>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => {
                    if count >= 0 {
                        let n = (count as usize).min(z.len());
                        Ok(z.by_score.keys().take(n).map(|(_, m)| m.clone()).collect())
                    } else {
                        let n = count.unsigned_abs() as usize;
                        let all: Vec<&String> = z.by_score.keys().map(|(_, m)| m).collect();
                        Ok((0..n).map(|i| all[i % all.len()].clone()).collect())
                    }
                }
                _ => Err(Error::WrongType),
            },
        }
    }

    fn get_zset_clone(&mut self, key: &str) -> Result<ZSetInner> {
        match self.get(key) {
            None => Ok(ZSetInner::default()),
            Some(entry) => match &entry.value {
                Value::ZSet(z) => Ok(z.clone()),
                _ => Err(Error::WrongType),
            },
        }
    }

    pub fn zunionstore(&mut self, dest: &str, keys: &[String]) -> Result<usize> {
        let mut result = ZSetInner::default();
        for key in keys {
            let z = self.get_zset_clone(key)?;
            for (member, score) in z.scores {
                let new_score = result.scores.get(&member).copied().unwrap_or(0.0) + score;
                result.upsert(member, new_score);
            }
        }
        let len = result.len();
        if len == 0 {
            self.data.remove(dest);
        } else {
            self.data.insert(
                dest.to_string(),
                Entry {
                    value: Value::ZSet(result),
                    expires_at: None,
                },
            );
        }
        Ok(len)
    }

    pub fn zinterstore(&mut self, dest: &str, keys: &[String]) -> Result<usize> {
        if keys.is_empty() {
            self.data.remove(dest);
            return Ok(0);
        }
        let mut sets: Vec<ZSetInner> = Vec::with_capacity(keys.len());
        for key in keys {
            sets.push(self.get_zset_clone(key)?);
        }
        let (first, rest) = sets.split_first().unwrap();
        let mut result = ZSetInner::default();
        for (member, &score) in &first.scores {
            if rest.iter().all(|z| z.scores.contains_key(member.as_str())) {
                let sum = rest.iter().fold(score, |acc, z| {
                    acc + z.scores.get(member.as_str()).copied().unwrap_or(0.0)
                });
                result.upsert(member.clone(), sum);
            }
        }
        let len = result.len();
        if len == 0 {
            self.data.remove(dest);
        } else {
            self.data.insert(
                dest.to_string(),
                Entry {
                    value: Value::ZSet(result),
                    expires_at: None,
                },
            );
        }
        Ok(len)
    }

    /// Serialize all non-expired entries to AOF bytes (RESP2 format).
    /// Used by BGREWRITEAOF to produce a compact snapshot.
    pub fn snapshot_aof_bytes(&mut self) -> Vec<u8> {
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        let now = Instant::now();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Eagerly evict expired keys so they are not snapshotted.
        self.purge_expired();

        let mut out: Vec<u8> = Vec::new();

        for (key, entry) in &self.data {
            let pxat = entry.expires_at.map(|inst| {
                let remaining_ms = inst.saturating_duration_since(now).as_millis() as u64;
                now_ms.saturating_add(remaining_ms)
            });

            match &entry.value {
                Value::Str(b) => {
                    let argc = if pxat.is_some() { 5 } else { 3 };
                    snap_array(&mut out, argc);
                    snap_bulk(&mut out, b"SET");
                    snap_bulk(&mut out, key.as_bytes());
                    snap_bulk(&mut out, b);
                    if let Some(ms) = pxat {
                        snap_bulk(&mut out, b"PXAT");
                        snap_bulk(&mut out, ms.to_string().as_bytes());
                    }
                }
                Value::List(list) => {
                    if list.is_empty() {
                        continue;
                    }
                    snap_array(&mut out, 2 + list.len());
                    snap_bulk(&mut out, b"RPUSH");
                    snap_bulk(&mut out, key.as_bytes());
                    for elem in list {
                        snap_bulk(&mut out, elem);
                    }
                    if let Some(ms) = pxat {
                        snap_pexpireat(&mut out, key, ms);
                    }
                }
                Value::Hash(h) => {
                    if h.is_empty() {
                        continue;
                    }
                    snap_array(&mut out, 2 + h.len() * 2);
                    snap_bulk(&mut out, b"HSET");
                    snap_bulk(&mut out, key.as_bytes());
                    for (field, val) in h {
                        snap_bulk(&mut out, field.as_bytes());
                        snap_bulk(&mut out, val);
                    }
                    if let Some(ms) = pxat {
                        snap_pexpireat(&mut out, key, ms);
                    }
                }
                Value::Set(s) => {
                    if s.is_empty() {
                        continue;
                    }
                    snap_array(&mut out, 2 + s.len());
                    snap_bulk(&mut out, b"SADD");
                    snap_bulk(&mut out, key.as_bytes());
                    for member in s {
                        snap_bulk(&mut out, member);
                    }
                    if let Some(ms) = pxat {
                        snap_pexpireat(&mut out, key, ms);
                    }
                }
                Value::ZSet(z) => {
                    if z.is_empty() {
                        continue;
                    }
                    snap_array(&mut out, 2 + z.scores.len() * 2);
                    snap_bulk(&mut out, b"ZADD");
                    snap_bulk(&mut out, key.as_bytes());
                    for ((score, member), ()) in &z.by_score {
                        snap_bulk(&mut out, snap_score(score.0).as_bytes());
                        snap_bulk(&mut out, member.as_bytes());
                    }
                    if let Some(ms) = pxat {
                        snap_pexpireat(&mut out, key, ms);
                    }
                }
            }
        }

        out
    }

    // ── memory / eviction ────────────────────────────────────────────────────

    fn next_lru_tick(&mut self) -> u64 {
        self.lru_tick = self.lru_tick.wrapping_add(1);
        self.lru_tick
    }

    /// Rough estimate of memory used by stored data, in bytes.
    pub fn rough_memory_usage(&self) -> u64 {
        const ENTRY_OVERHEAD: u64 = 64; // HashMap node + Entry struct
        self.data
            .iter()
            .map(|(k, e)| {
                let val = match &e.value {
                    Value::Str(b) => b.len() as u64,
                    Value::List(l) => l.iter().map(|e| e.len() as u64 + 16).sum::<u64>(),
                    Value::Hash(h) => h
                        .iter()
                        .map(|(f, v)| f.len() as u64 + v.len() as u64 + 32)
                        .sum::<u64>(),
                    Value::Set(s) => s.iter().map(|m| m.len() as u64 + 16).sum::<u64>(),
                    Value::ZSet(z) => z.scores.keys().map(|m| m.len() as u64 + 32).sum::<u64>(),
                };
                k.len() as u64 + val + ENTRY_OVERHEAD
            })
            .sum()
    }

    /// Evict keys until memory usage drops below `maxmemory` (or policy is NoEviction).
    /// Returns `Err` if the policy is `noeviction` and we're over the limit.
    pub fn evict_if_needed(&mut self) -> Result<()> {
        if self.maxmemory == 0 {
            return Ok(());
        }
        // Free expired entries first — they're wasted memory.
        self.purge_expired();
        while self.rough_memory_usage() > self.maxmemory {
            if !self.evict_one() {
                return Err(Error::Protocol(
                    "OOM command not allowed when used memory > 'maxmemory'. \
                     Use OBJECT HELP for suggestions."
                        .into(),
                ));
            }
        }
        Ok(())
    }

    /// Remove one key according to the current eviction policy.
    /// Returns `false` if nothing can be evicted (empty keyspace or NoEviction).
    fn evict_one(&mut self) -> bool {
        const SAMPLE: usize = 5;
        match self.eviction_policy {
            EvictionPolicy::NoEviction => false,

            EvictionPolicy::AllKeysRandom => {
                if let Some(key) = self.data.keys().next().cloned() {
                    self.data.remove(&key);
                    self.lru_clock.remove(&key);
                    true
                } else {
                    false
                }
            }

            EvictionPolicy::VolatileRandom => {
                let key = self
                    .data
                    .iter()
                    .find(|(_, e)| e.expires_at.is_some() && !e.is_expired())
                    .map(|(k, _)| k.clone());
                if let Some(k) = key {
                    self.data.remove(&k);
                    self.lru_clock.remove(&k);
                    true
                } else {
                    false
                }
            }

            EvictionPolicy::AllKeysLru => {
                // Sample SAMPLE keys; evict the one with the lowest (oldest) LRU tick.
                let sample: Vec<String> = self.data.keys().take(SAMPLE).cloned().collect();
                let victim = sample
                    .into_iter()
                    .min_by_key(|k| self.lru_clock.get(k).copied().unwrap_or(0));
                if let Some(key) = victim {
                    self.data.remove(&key);
                    self.lru_clock.remove(&key);
                    true
                } else {
                    false
                }
            }

            EvictionPolicy::VolatileLru => {
                let sample: Vec<String> = self
                    .data
                    .iter()
                    .filter(|(_, e)| e.expires_at.is_some() && !e.is_expired())
                    .take(SAMPLE)
                    .map(|(k, _)| k.clone())
                    .collect();
                let victim = sample
                    .into_iter()
                    .min_by_key(|k| self.lru_clock.get(k).copied().unwrap_or(0));
                if let Some(key) = victim {
                    self.data.remove(&key);
                    self.lru_clock.remove(&key);
                    true
                } else {
                    false
                }
            }

            EvictionPolicy::VolatileTtl => {
                // Evict the key with the smallest remaining TTL.
                let victim = self
                    .data
                    .iter()
                    .filter(|(_, e)| e.expires_at.is_some() && !e.is_expired())
                    .min_by_key(|(_, e)| e.expires_at.unwrap())
                    .map(|(k, _)| k.clone());
                if let Some(key) = victim {
                    self.data.remove(&key);
                    self.lru_clock.remove(&key);
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ListDirection {
    Left,
    Right,
}

fn normalize_index(idx: i64, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let len = len as i64;
    let i = if idx < 0 { len + idx } else { idx };
    if i < 0 || i >= len {
        None
    } else {
        Some(i as usize)
    }
}

fn resolve_range(start: i64, stop: i64, len: usize) -> Option<(usize, usize)> {
    if len == 0 {
        return None;
    }
    let len = len as i64;
    let s = if start < 0 {
        (len + start).max(0)
    } else {
        start
    };
    let e = if stop < 0 { len + stop } else { stop }.min(len - 1);
    if s > e || e < 0 {
        None
    } else {
        Some((s as usize, e as usize))
    }
}

fn format_decimal(d: rust_decimal::Decimal) -> String {
    // normalize() drops trailing fractional zeros: "10.60" → "10.6", "10.00" → "10"
    d.normalize().to_string()
}

// ── snapshot helpers ──────────────────────────────────────────────────────────

fn snap_array(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(format!("*{n}\r\n").as_bytes());
}

fn snap_bulk(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(format!("${}\r\n", s.len()).as_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(b"\r\n");
}

fn snap_pexpireat(out: &mut Vec<u8>, key: &str, epoch_ms: u64) {
    snap_array(out, 3);
    snap_bulk(out, b"PEXPIREAT");
    snap_bulk(out, key.as_bytes());
    snap_bulk(out, epoch_ms.to_string().as_bytes());
}

fn snap_score(score: f64) -> String {
    if score == f64::INFINITY {
        "inf".to_string()
    } else if score == f64::NEG_INFINITY {
        "-inf".to_string()
    } else {
        score.to_string()
    }
}
