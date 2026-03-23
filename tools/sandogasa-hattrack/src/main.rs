// SPDX-License-Identifier: MPL-2.0

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    before_help = concat!(
        env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")
    )
)]
struct Cli {
    /// Output machine-readable JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show a contributor's Discourse profile and activity
    Discourse {
        /// Discourse username to look up
        username: String,

        /// Discourse instance base URL
        #[arg(long, default_value = "https://discussion.fedoraproject.org")]
        url: String,
    },
}

#[derive(Serialize)]
struct DiscourseProfile {
    username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_posted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<DiscourseStatus>,
}

#[derive(Serialize)]
struct DiscourseStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ends_at: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let json = cli.json;
    match cli.command {
        Command::Discourse { username, url } => cmd_discourse(&username, &url, json).await,
    }
}

async fn cmd_discourse(
    username: &str,
    base_url: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = sandogasa_discourse::DiscourseClient::new(base_url);
    let user = client.user(username).await?;

    let profile = DiscourseProfile {
        username: user.username.clone(),
        name: user.name.clone(),
        title: user.title.clone(),
        timezone: user.timezone.clone(),
        location: user.location.clone(),
        last_posted_at: user.last_posted_at.map(|t| t.to_rfc3339()),
        last_seen_at: user.last_seen_at.map(|t| t.to_rfc3339()),
        status: user.status.as_ref().map(|s| DiscourseStatus {
            emoji: s.emoji.clone(),
            description: s.description.clone(),
            ends_at: s.ends_at.map(|t| t.to_rfc3339()),
        }),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&profile)?);
        return Ok(());
    }

    println!("Discourse: {}", user.username);
    if let Some(name) = &user.name {
        println!("  Name:        {name}");
    }
    if let Some(title) = &user.title {
        println!("  Title:       {title}");
    }
    if let Some(tz) = &user.timezone {
        println!("  Timezone:    {tz}");
    }
    if let Some(loc) = &user.location {
        println!("  Location:    {loc}");
    }
    if let Some(ts) = &user.last_posted_at {
        println!("  Last post:   {ts}");
    }
    if let Some(ts) = &user.last_seen_at {
        println!("  Last seen:   {ts}");
    }
    if let Some(status) = &user.status {
        let emoji = status.emoji.as_deref().unwrap_or("");
        let desc = status.description.as_deref().unwrap_or("");
        if !emoji.is_empty() || !desc.is_empty() {
            println!("  Status:      {emoji} {desc}");
        }
        if let Some(ends) = &status.ends_at {
            println!("  Status ends: {ends}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discourse_profile_serializes_full() {
        let profile = DiscourseProfile {
            username: "mattdm".to_string(),
            name: Some("Matthew Miller".to_string()),
            title: Some("Fedora Project Leader".to_string()),
            timezone: Some("America/New_York".to_string()),
            location: Some("Somerville, MA".to_string()),
            last_posted_at: Some("2026-03-17T14:50:30+00:00".to_string()),
            last_seen_at: Some("2026-03-22T05:36:12+00:00".to_string()),
            status: Some(DiscourseStatus {
                emoji: Some("palm_tree".to_string()),
                description: Some("On vacation".to_string()),
                ends_at: Some("2026-04-01T00:00:00+00:00".to_string()),
            }),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&profile).unwrap()).unwrap();
        assert_eq!(json["username"], "mattdm");
        assert_eq!(json["timezone"], "America/New_York");
        assert_eq!(json["location"], "Somerville, MA");
        assert_eq!(json["status"]["emoji"], "palm_tree");
        assert_eq!(json["status"]["description"], "On vacation");
    }

    #[test]
    fn discourse_profile_omits_none_fields() {
        let profile = DiscourseProfile {
            username: "newuser".to_string(),
            name: None,
            title: None,
            timezone: None,
            location: None,
            last_posted_at: None,
            last_seen_at: None,
            status: None,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&profile).unwrap()).unwrap();
        assert_eq!(json["username"], "newuser");
        assert!(json.get("timezone").is_none());
        assert!(json.get("location").is_none());
        assert!(json.get("status").is_none());
        assert!(json.get("last_posted_at").is_none());
    }

    #[test]
    fn discourse_status_omits_none_fields() {
        let status = DiscourseStatus {
            emoji: Some("coffee".to_string()),
            description: None,
            ends_at: None,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&status).unwrap()).unwrap();
        assert_eq!(json["emoji"], "coffee");
        assert!(json.get("description").is_none());
        assert!(json.get("ends_at").is_none());
    }
}
