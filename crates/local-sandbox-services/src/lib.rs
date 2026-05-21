//! Local service orchestration boundary.

use serde::{Deserialize, Serialize};

/// Local service kind managed by the substrate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalServiceKind {
    /// S3-compatible object storage.
    S3,
    /// `PostgreSQL` database.
    Postgres,
}
