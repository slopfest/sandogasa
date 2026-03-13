// SPDX-License-Identifier: MPL-2.0

use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct BugSearchResponse {
    pub bugs: Vec<Bug>,
    pub total_matches: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Bug {
    pub id: u64,
    pub summary: String,
    pub status: String,
    pub resolution: String,
    pub product: String,
    pub component: Vec<String>,
    pub severity: String,
    pub priority: String,
    pub assigned_to: String,
    pub creator: String,
    pub creation_time: DateTime<Utc>,
    pub last_change_time: DateTime<Utc>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub alias: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<u64>,
    #[serde(default)]
    pub blocks: Vec<u64>,
    #[serde(default)]
    pub see_also: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub flags: Vec<Flag>,
    #[serde(default)]
    pub version: Vec<String>,
    #[serde(default)]
    pub cf_fixed_in: String,
}

#[derive(Debug, Deserialize)]
pub struct Flag {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub setter: String,
    #[serde(default)]
    pub requestee: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CommentResponse {
    pub bugs: std::collections::HashMap<String, CommentBucket>,
}

#[derive(Debug, Deserialize)]
pub struct CommentBucket {
    pub comments: Vec<Comment>,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub text: String,
    pub creator: String,
    pub creation_time: DateTime<Utc>,
    pub is_private: bool,
}
