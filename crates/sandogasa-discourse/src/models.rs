// SPDX-License-Identifier: MPL-2.0

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Top-level response from `GET /u/{username}.json`.
#[derive(Debug, Deserialize)]
pub struct UserResponse {
    pub user: User,
}

/// A Discourse user profile.
#[derive(Debug, Deserialize)]
pub struct User {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub last_posted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub status: Option<UserStatus>,
}

/// Custom status set by the user (emoji + description, with optional expiry).
#[derive(Debug, Deserialize)]
pub struct UserStatus {
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub ends_at: Option<DateTime<Utc>>,
}
