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
    /// Show a contributor's recent Bodhi updates and comments
    Bodhi {
        /// Bodhi/FAS username to look up
        username: String,

        /// Bodhi instance base URL
        #[arg(long, default_value = "https://bodhi.fedoraproject.org")]
        url: String,
    },

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
        Command::Bodhi { username, url } => cmd_bodhi(&username, &url, json).await,
        Command::Discourse { username, url } => cmd_discourse(&username, &url, json).await,
    }
}

#[derive(Serialize)]
struct BodhiActivity {
    username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_update: Option<BodhiUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_comment: Option<BodhiComment>,
}

#[derive(Serialize)]
struct BodhiUpdate {
    alias: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    release: Option<String>,
    builds: Vec<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_submitted: Option<String>,
}

#[derive(Serialize)]
struct BodhiComment {
    #[serde(skip_serializing_if = "Option::is_none")]
    update_alias: Option<String>,
    karma: i32,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

async fn cmd_bodhi(
    username: &str,
    base_url: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = sandogasa_bodhi::BodhiClient::with_base_url(base_url);
    let (updates, comments) = tokio::try_join!(
        client.updates_for_user(username, 1),
        client.comments_for_user(username, 1),
    )?;

    let last_update = updates.into_iter().next().map(|u| BodhiUpdate {
        alias: u.alias,
        release: u.release.map(|r| r.name),
        builds: u.builds.into_iter().map(|b| b.nvr).collect(),
        status: u.status,
        date_submitted: u.date_submitted,
    });

    let last_comment = comments.into_iter().next().map(|c| BodhiComment {
        update_alias: c.update_alias,
        karma: c.karma,
        text: c.text,
        timestamp: c.timestamp,
    });

    let activity = BodhiActivity {
        username: username.to_string(),
        last_update,
        last_comment,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&activity)?);
        return Ok(());
    }

    println!("Bodhi: {username}");

    if let Some(update) = &activity.last_update {
        println!("\n  Last update: {}", update.alias);
        if let Some(release) = &update.release {
            println!("    Release:   {release}");
        }
        for nvr in &update.builds {
            println!("    Build:     {nvr}");
        }
        println!("    Status:    {}", update.status);
        if let Some(ts) = &update.date_submitted {
            println!("    Submitted: {ts}");
        }
    } else {
        println!("\n  No updates found.");
    }

    if let Some(comment) = &activity.last_comment {
        let karma_str = match comment.karma {
            k if k > 0 => format!("+{k}"),
            k if k < 0 => format!("{k}"),
            _ => "0".to_string(),
        };
        println!("\n  Last comment:");
        if let Some(alias) = &comment.update_alias {
            println!("    Update:    {alias}");
        }
        println!("    Karma:     {karma_str}");
        if let Some(ts) = &comment.timestamp {
            println!("    Date:      {ts}");
        }
        // Show first line of comment text as a preview
        let preview = comment.text.lines().next().unwrap_or("");
        if !preview.is_empty() {
            if comment.text.lines().count() > 1 {
                println!("    Text:      {preview} [...]");
            } else {
                println!("    Text:      {preview}");
            }
        }
    } else {
        println!("\n  No comments found.");
    }

    Ok(())
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
        let emoji = status
            .emoji
            .as_deref()
            .and_then(render_emoji)
            .unwrap_or_default();
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

/// Convert a Discourse emoji shortcode to a Unicode emoji string.
fn render_emoji(shortcode: &str) -> Option<&'static str> {
    emojis::get_by_shortcode(shortcode).map(|e| e.as_str())
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

    // ---- Bodhi serialization ----

    #[test]
    fn bodhi_activity_serializes_full() {
        let activity = BodhiActivity {
            username: "salimma".to_string(),
            last_update: Some(BodhiUpdate {
                alias: "FEDORA-2026-b600f85be9".to_string(),
                release: Some("F44".to_string()),
                builds: vec!["python-puzpy-0.5.0-2.fc44".to_string()],
                status: "testing".to_string(),
                date_submitted: Some("2026-03-20 23:44:44".to_string()),
            }),
            last_comment: Some(BodhiComment {
                update_alias: Some("FEDORA-EPEL-2026-8e235e20a2".to_string()),
                karma: 1,
                text: "Works for me".to_string(),
                timestamp: Some("2026-02-24 11:17:59".to_string()),
            }),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert_eq!(json["username"], "salimma");
        assert_eq!(json["last_update"]["alias"], "FEDORA-2026-b600f85be9");
        assert_eq!(json["last_update"]["release"], "F44");
        assert_eq!(
            json["last_update"]["builds"][0],
            "python-puzpy-0.5.0-2.fc44"
        );
        assert_eq!(json["last_comment"]["karma"], 1);
        assert_eq!(
            json["last_comment"]["update_alias"],
            "FEDORA-EPEL-2026-8e235e20a2"
        );
    }

    #[test]
    fn bodhi_activity_omits_none_fields() {
        let activity = BodhiActivity {
            username: "nobody".to_string(),
            last_update: None,
            last_comment: None,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert_eq!(json["username"], "nobody");
        assert!(json.get("last_update").is_none());
        assert!(json.get("last_comment").is_none());
    }

    #[test]
    fn bodhi_comment_negative_karma_serializes() {
        let comment = BodhiComment {
            update_alias: Some("FEDORA-2026-xyz".to_string()),
            karma: -1,
            text: "Broken on aarch64".to_string(),
            timestamp: None,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&comment).unwrap()).unwrap();
        assert_eq!(json["karma"], -1);
        assert!(json.get("timestamp").is_none());
    }

    // ---- render_emoji ----

    #[test]
    fn render_emoji_known_shortcode() {
        assert_eq!(render_emoji("palm_tree"), Some("🌴"));
    }

    #[test]
    fn render_emoji_another_shortcode() {
        assert_eq!(render_emoji("rocket"), Some("🚀"));
    }

    #[test]
    fn render_emoji_unknown_shortcode() {
        assert_eq!(render_emoji("not_a_real_emoji_xyz"), None);
    }
}
