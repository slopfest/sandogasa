// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::io::{self, Write as _};
use std::process::ExitCode;

use chrono::{DateTime, NaiveDateTime, Utc};
use clap::{Parser, Subcommand};
use sandogasa_fasjson::kerberos;
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

    /// Show a contributor's recent Bugzilla activity
    Bugzilla {
        /// FAS username to look up (used to discover email via FASJSON)
        #[arg(required_unless_present = "email")]
        username: Option<String>,

        /// Email address(es) to search for.
        /// Repeat for multiple: --email a@x.org --email b@y.org
        #[arg(long)]
        email: Vec<String>,

        /// Skip FASJSON lookup, only search username@fedoraproject.org
        #[arg(long)]
        no_fas: bool,

        /// Bugzilla instance base URL
        #[arg(long, default_value = "https://bugzilla.redhat.com")]
        url: String,
    },

    /// Show a contributor's recent dist-git activity
    Distgit {
        /// FAS/Pagure username to look up
        username: String,

        /// Dist-git instance base URL
        #[arg(long, default_value = "https://src.fedoraproject.org")]
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

    /// Summary of a contributor's last activity across all services
    LastSeen {
        /// FAS username to look up
        username: String,

        /// Email address(es) for Bugzilla/Mailman.
        /// Repeat for multiple: --email a@x.org --email b@y.org
        #[arg(long)]
        email: Vec<String>,

        /// Skip FASJSON lookup for email discovery
        #[arg(long)]
        no_fas: bool,

        /// Mailing list(s) to search for sender ID
        #[arg(long, default_value = "devel@lists.fedoraproject.org")]
        list: Vec<String>,

        /// Max pages to scan mailing list archives
        #[arg(long, default_value = "200")]
        max_pages: u32,
    },

    /// Show a contributor's mailing list activity
    Mailman {
        /// FAS username to look up (used to discover email via FASJSON)
        #[arg(required_unless_present = "email")]
        username: Option<String>,

        /// Email address(es) to search for (skips FASJSON lookup).
        /// Repeat for multiple: --email a@x.org --email b@y.org
        #[arg(long)]
        email: Vec<String>,

        /// Skip FASJSON lookup, only search username@fedoraproject.org
        #[arg(long)]
        no_fas: bool,

        /// Mailing list(s) to search for the sender.
        /// Repeat for multiple: --list a@x.org --list b@x.org
        #[arg(long, default_value = "devel@lists.fedoraproject.org")]
        list: Vec<String>,

        /// Max pages to scan when searching for sender
        /// (each page has ~10 emails; high-traffic lists
        /// may need 200+ to cover a week)
        #[arg(long, default_value = "200")]
        max_pages: u32,
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
        Command::Bugzilla {
            username,
            email,
            no_fas,
            url,
        } => cmd_bugzilla(username.as_deref(), &email, no_fas, &url, json).await,
        Command::Distgit { username, url } => cmd_distgit(&username, &url, json).await,
        Command::Discourse { username, url } => cmd_discourse(&username, &url, json).await,
        Command::LastSeen {
            username,
            email,
            no_fas,
            list,
            max_pages,
        } => cmd_last_seen(&username, &email, no_fas, &list, max_pages, json).await,
        Command::Mailman {
            username,
            email,
            no_fas,
            list,
            max_pages,
        } => cmd_mailman(username.as_deref(), &email, no_fas, &list, max_pages, json).await,
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
            if let Some(dt) = parse_bodhi_timestamp(ts) {
                println!("    Submitted: {}", format_with_relative(ts, dt));
            } else {
                println!("    Submitted: {ts}");
            }
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
            if let Some(dt) = parse_bodhi_timestamp(ts) {
                println!("    Date:      {}", format_with_relative(ts, dt));
            } else {
                println!("    Date:      {ts}");
            }
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

#[derive(Serialize)]
struct BugzillaActivity {
    emails_searched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_filed: Option<BugSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_changed: Option<BugSummary>,
}

#[derive(Serialize)]
struct BugSummary {
    id: u64,
    summary: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
}

async fn cmd_bugzilla(
    username: Option<&str>,
    email_overrides: &[String],
    no_fas: bool,
    base_url: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let emails = resolve_emails(username, email_overrides, no_fas)?;

    let client = sandogasa_bugzilla::BzClient::new(base_url);

    let mut last_filed = None;
    let mut last_changed = None;

    for email in &emails {
        // Last bug filed by this email
        if last_filed.is_none() {
            let bugs = client
                .search(&format!("creator={email}&limit=1&order=bug_id%20DESC"), 1)
                .await?;
            if let Some(bug) = bugs.into_iter().next() {
                last_filed = Some(BugSummary {
                    id: bug.id,
                    summary: bug.summary,
                    status: bug.status,
                    component: bug.component.into_iter().next(),
                    date: Some(bug.creation_time.to_rfc3339()),
                });
            }
        }

        // Last bug changed by this email
        if last_changed.is_none() {
            let bugs = client
                .search(
                    &format!(
                        "f1=commenter&o1=equals&v1={email}\
                         &limit=1&order=last_change_time%20DESC"
                    ),
                    1,
                )
                .await?;
            if let Some(bug) = bugs.into_iter().next() {
                last_changed = Some(BugSummary {
                    id: bug.id,
                    summary: bug.summary,
                    status: bug.status,
                    component: bug.component.into_iter().next(),
                    date: Some(bug.last_change_time.to_rfc3339()),
                });
            }
        }

        if last_filed.is_some() && last_changed.is_some() {
            break;
        }
    }

    let activity = BugzillaActivity {
        emails_searched: emails.clone(),
        last_filed,
        last_changed,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&activity)?);
        return Ok(());
    }

    println!("Bugzilla: {}", emails.join(", "));

    if let Some(bug) = &activity.last_filed {
        let component = bug.component.as_deref().unwrap_or("?");
        println!("\n  Last bug filed:");
        println!("    #{} [{}] {}", bug.id, bug.status, bug.summary);
        println!("    Component: {component}");
        if let Some(ts) = &bug.date {
            if let Ok(dt) = ts.parse::<DateTime<Utc>>() {
                println!("    Filed:     {}", format_with_relative(ts, dt));
            } else {
                println!("    Filed:     {ts}");
            }
        }
    } else {
        println!("\n  No bugs filed.");
    }

    if let Some(bug) = &activity.last_changed {
        let component = bug.component.as_deref().unwrap_or("?");
        println!("\n  Last bug changed:");
        println!("    #{} [{}] {}", bug.id, bug.status, bug.summary);
        println!("    Component: {component}");
        if let Some(ts) = &bug.date {
            if let Ok(dt) = ts.parse::<DateTime<Utc>>() {
                println!("    Changed:   {}", format_with_relative(ts, dt));
            } else {
                println!("    Changed:   {ts}");
            }
        }
    } else {
        println!("\n  No bug changes found.");
    }

    Ok(())
}

#[derive(Serialize)]
struct MailmanActivity {
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    emails_searched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mailman_id: Option<String>,
    recent_posts: Vec<MailmanPost>,
}

#[derive(Serialize)]
struct MailmanPost {
    subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    list: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
}

/// Resolve email addresses to search for: use --email if provided, otherwise
/// query FASJSON (with Kerberos) to discover emails from the FAS username.
/// Always includes `username@fedoraproject.org` since users may post from
/// either their FAS alias or their personal email.
fn resolve_emails(
    username: Option<&str>,
    email_overrides: &[String],
    no_fas: bool,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if !email_overrides.is_empty() {
        return Ok(email_overrides.to_vec());
    }

    let fas_username = username.ok_or("either a username or --email must be provided")?;

    let mut emails = vec![format!("{fas_username}@fedoraproject.org")];

    if no_fas {
        return Ok(emails);
    }

    // Try FASJSON for additional email addresses
    match ensure_kerberos_ticket() {
        Ok(()) => {
            eprintln!("Looking up {fas_username} in FASJSON...");
            let client = sandogasa_fasjson::FasjsonClient::new();
            match client.user(fas_username) {
                Ok(user) => {
                    for email in user.emails {
                        if !emails.contains(&email) {
                            emails.push(email);
                        }
                    }
                    if let Some(rhbz) = user.rhbzemail
                        && !emails.contains(&rhbz)
                    {
                        emails.push(rhbz);
                    }
                }
                Err(e) => {
                    eprintln!("FASJSON lookup failed: {e}");
                    eprintln!("Continuing with {fas_username}@fedoraproject.org only.");
                }
            }
        }
        Err(e) => {
            eprintln!("Kerberos: {e}");
            eprintln!("Continuing with {fas_username}@fedoraproject.org only.");
        }
    }

    Ok(emails)
}

/// Ensure a valid Kerberos ticket exists, offering to renew or acquire one.
fn ensure_kerberos_ticket() -> Result<(), Box<dyn std::error::Error>> {
    match kerberos::ticket_status() {
        kerberos::TicketStatus::Valid => Ok(()),
        kerberos::TicketStatus::ExpiredRenewable => {
            eprint!("Kerberos ticket expired but renewable. Renewing... ");
            io::stderr().flush()?;
            match kerberos::renew_ticket() {
                Ok(()) => {
                    eprintln!("OK");
                    Ok(())
                }
                Err(_) => {
                    eprintln!("failed");
                    eprintln!("Ticket is no longer renewable. Acquiring a new one.");
                    acquire_new_ticket()
                }
            }
        }
        kerberos::TicketStatus::None => {
            eprintln!("No valid Kerberos ticket found.");
            acquire_new_ticket()
        }
    }
}

/// Acquire a new Kerberos ticket, reading the principal from ~/.fedora.upn.
///
/// Retries on failure since the Fedora KDC can be slow to respond
/// while also timing out aggressively on password input.
fn acquire_new_ticket() -> Result<(), Box<dyn std::error::Error>> {
    let upn = kerberos::read_fedora_upn()
        .ok_or("no ~/.fedora.upn found — cannot determine Kerberos principal")?;
    let principal = format!("{upn}@FEDORAPROJECT.ORG");

    loop {
        eprintln!("Running kinit {principal}...");
        match kerberos::acquire_ticket(&principal) {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("kinit failed: {e}");
                eprint!("Try again? [Y/n] ");
                io::stderr().flush()?;
                let mut answer = String::new();
                io::stdin().read_line(&mut answer)?;
                let answer = answer.trim();
                if !answer.is_empty() && !answer.eq_ignore_ascii_case("y") {
                    return Err("Kerberos authentication cancelled".into());
                }
            }
        }
    }
}

/// Extract a mailing list name from a HyperKitty API URL.
///
/// Converts `https://lists.fedoraproject.org/archives/api/list/devel@lists.fedoraproject.org/`
/// to `devel@lists.fedoraproject.org`.
fn extract_list_name(api_url: &str) -> String {
    // Look for /api/list/{name}/ pattern
    if let Some(rest) = api_url.split("/api/list/").nth(1) {
        rest.split('/').next().unwrap_or(api_url).to_string()
    } else {
        api_url.to_string()
    }
}

async fn cmd_mailman(
    username: Option<&str>,
    email_overrides: &[String],
    no_fas: bool,
    lists: &[String],
    max_pages: u32,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let emails = resolve_emails(username, email_overrides, no_fas)?;

    let client = sandogasa_mailman::MailmanClient::new();

    // Search for sender across configured lists, trying each email address
    let mut mailman_id = None;
    for list in lists {
        let searching: Vec<_> = emails.iter().map(|e| e.as_str()).collect();
        eprintln!(
            "Searching {list} for {} (up to {max_pages} pages)...",
            searching.join(", ")
        );
        if let Some(id) = client.find_sender_id(list, &emails, max_pages).await? {
            mailman_id = Some(id);
            break;
        }
    }

    let recent_posts = if let Some(ref id) = mailman_id {
        let fetched = client.sender_emails(id, 5).await?;
        fetched
            .into_iter()
            .map(|e| MailmanPost {
                subject: e.subject,
                list: e.mailinglist.map(|u| extract_list_name(&u)),
                date: e.date,
            })
            .collect()
    } else {
        vec![]
    };

    let activity = MailmanActivity {
        username: username.map(String::from),
        emails_searched: emails.clone(),
        mailman_id: mailman_id.clone(),
        recent_posts,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&activity)?);
        return Ok(());
    }

    println!("Mailing lists: {}", emails.join(", "));
    if let Some(id) = &mailman_id {
        println!("  Sender ID: {id}");
    }

    if activity.recent_posts.is_empty() {
        println!("\n  No recent posts found.");
    } else {
        println!("\n  Recent posts:");
        for post in &activity.recent_posts {
            let list = post.list.as_deref().unwrap_or("?");
            let date_str = if let Some(ts) = &post.date {
                if let Ok(dt) = ts.parse::<DateTime<chrono::FixedOffset>>() {
                    format_with_relative(ts, dt.with_timezone(&Utc))
                } else {
                    ts.clone()
                }
            } else {
                "?".to_string()
            };
            println!("    [{list}] {}", post.subject);
            println!("      {date_str}");
        }
    }

    Ok(())
}

#[derive(Serialize)]
struct DistgitActivity {
    username: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    recent_days: Vec<DistgitDay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_pr: Option<DistgitPr>,
    actionable_prs: Vec<DistgitPr>,
    actionable_total: u64,
}

#[derive(Serialize)]
struct DistgitDay {
    date: String,
    actions: u64,
}

#[derive(Serialize)]
struct DistgitPr {
    id: u64,
    title: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_created: Option<String>,
}

/// Convert a Unix timestamp string to RFC 3339.
fn unix_to_rfc3339(ts: &str) -> Option<String> {
    let secs: i64 = ts.parse().ok()?;
    DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
}

async fn cmd_distgit(
    username: &str,
    base_url: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = sandogasa_distgit::DistGitClient::with_base_url(base_url);

    let (stats, prs, actionable) = tokio::try_join!(
        client.user_activity_stats(username),
        client.user_pull_requests(username, "all", 1),
        client.user_actionable_pull_requests(username, 10),
    )?;
    let (actionable_prs, actionable_total) = actionable;

    // Last 7 days of activity, sorted most recent first
    let mut days: Vec<_> = stats.into_iter().collect();
    days.sort_by(|a, b| b.0.cmp(&a.0));
    let recent_days: Vec<DistgitDay> = days
        .into_iter()
        .take(7)
        .map(|(date, actions)| DistgitDay { date, actions })
        .collect();

    let last_pr = prs.into_iter().next().map(|pr| DistgitPr {
        id: pr.id,
        title: pr.title,
        status: pr.status,
        project: pr.project.map(|p| p.fullname),
        date_created: pr.date_created.as_deref().and_then(unix_to_rfc3339),
    });

    let activity = DistgitActivity {
        username: username.to_string(),
        recent_days,
        last_pr,
        actionable_prs: actionable_prs
            .into_iter()
            .map(|pr| DistgitPr {
                id: pr.id,
                title: pr.title,
                status: pr.status,
                project: pr.project.map(|p| p.fullname),
                date_created: pr.date_created.as_deref().and_then(unix_to_rfc3339),
            })
            .collect(),
        actionable_total,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&activity)?);
        return Ok(());
    }

    println!("Dist-git: {username}");

    if !activity.recent_days.is_empty() {
        println!("\n  Recent activity:");
        let mut last_relative_week = None;
        for day in &activity.recent_days {
            if let Ok(date) = NaiveDateTime::parse_from_str(
                &format!("{} 00:00:00", day.date),
                "%Y-%m-%d %H:%M:%S",
            ) {
                let dt = date.and_utc();
                let days_ago = Utc::now().signed_duration_since(dt).num_days();
                let week = days_ago / 7;
                if last_relative_week != Some(week) {
                    last_relative_week = Some(week);
                    let rel = relative_time(dt);
                    println!("    {}: {} action(s)  ({rel})", day.date, day.actions);
                } else {
                    println!("    {}: {} action(s)", day.date, day.actions);
                }
            } else {
                println!("    {}: {} action(s)", day.date, day.actions);
            }
        }
    } else {
        println!("\n  No recent activity.");
    }

    if let Some(pr) = &activity.last_pr {
        let project = pr.project.as_deref().unwrap_or("?");
        println!("\n  Last PR filed:");
        println!("    #{} [{}] {}", pr.id, pr.status, pr.title);
        println!("    Project: {project}");
        if let Some(ts) = &pr.date_created {
            if let Ok(dt) = ts.parse::<DateTime<Utc>>() {
                println!("    Filed:   {}", format_with_relative(ts, dt));
            } else {
                println!("    Filed:   {ts}");
            }
        }
    } else {
        println!("\n  No PRs filed.");
    }

    if activity.actionable_total > 0 {
        println!("\n  {} PR(s) awaiting review:", activity.actionable_total);
        for pr in &activity.actionable_prs {
            let project = pr.project.as_deref().unwrap_or("?");
            println!("    #{} [{}] {}", pr.id, project, pr.title);
        }
        if activity.actionable_total > activity.actionable_prs.len() as u64 {
            println!(
                "    ... and {} more",
                activity.actionable_total - activity.actionable_prs.len() as u64
            );
        }
    }

    Ok(())
}

#[derive(Serialize)]
struct LastSeenSummary {
    username: String,
    services: Vec<ServiceLastSeen>,
}

#[derive(Serialize)]
struct ServiceLastSeen {
    service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_active: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status_expires: Option<String>,
}

async fn cmd_last_seen(
    username: &str,
    email_overrides: &[String],
    no_fas: bool,
    lists: &[String],
    max_pages: u32,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let emails = resolve_emails(Some(username), email_overrides, no_fas)?;
    let mut services = Vec::new();

    // Discourse
    eprintln!("Checking Discourse...");
    services.push(check_discourse(username).await);

    // Bodhi
    eprintln!("Checking Bodhi...");
    services.push(check_bodhi(username).await);

    // Dist-git
    eprintln!("Checking dist-git...");
    services.push(check_distgit(username).await);

    // Bugzilla
    eprintln!("Checking Bugzilla...");
    services.push(check_bugzilla(&emails).await);

    // Mailman
    eprintln!("Checking mailing lists...");
    services.push(check_mailman(&emails, lists, max_pages).await);

    // Sort by most recent first (entries with dates before those without)
    services.sort_by(|a, b| b.last_active.cmp(&a.last_active));

    let summary = LastSeenSummary {
        username: username.to_string(),
        services,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("Last seen: {username}\n");
    for svc in &summary.services {
        if let Some(ts) = &svc.last_active {
            let rel = if let Ok(dt) = ts.parse::<DateTime<Utc>>() {
                format!(" ({})", relative_time(dt))
            } else if let Ok(dt) = ts.parse::<DateTime<chrono::FixedOffset>>() {
                format!(" ({})", relative_time(dt.with_timezone(&Utc)))
            } else {
                String::new()
            };
            let detail = svc.detail.as_deref().unwrap_or("");
            if detail.is_empty() {
                println!("  {:<14} {ts}{rel}", svc.service);
            } else {
                println!("  {:<14} {ts}{rel}", svc.service);
                println!("  {:<14} {detail}", "");
            }
            if let Some(status) = &svc.status {
                println!("  {:<14} {:<8} {status}", "", "status:");
            }
            if let Some(expires) = &svc.status_expires {
                println!("  {:<14} {:<8} {expires}", "", "expires:");
            }
        } else if let Some(err) = &svc.error {
            println!("  {:<14} error: {err}", svc.service);
        } else {
            println!("  {:<14} no activity found", svc.service);
        }
    }

    Ok(())
}

async fn check_discourse(username: &str) -> ServiceLastSeen {
    let client = sandogasa_discourse::DiscourseClient::new("https://discussion.fedoraproject.org");
    match client.user(username).await {
        Ok(user) => {
            let (status, status_expires) = match user.status.as_ref() {
                Some(s) => {
                    let emoji = s
                        .emoji
                        .as_deref()
                        .and_then(render_emoji)
                        .unwrap_or_default();
                    let desc = s.description.as_deref().unwrap_or("");
                    let text = format!("{emoji} {desc}").trim().to_string();
                    let expires = s.ends_at.map(|ends| {
                        format!(
                            "{} ({})",
                            ends.format("%Y-%m-%d %H:%M UTC"),
                            relative_time(ends)
                        )
                    });
                    (if text.is_empty() { None } else { Some(text) }, expires)
                }
                None => (None, None),
            };
            ServiceLastSeen {
                service: "Discourse".to_string(),
                last_active: user.last_posted_at.map(|t| t.to_rfc3339()),
                detail: user.last_posted_at.map(|_| "last post".to_string()),
                error: None,
                status,
                status_expires,
            }
        }
        Err(e) => ServiceLastSeen {
            service: "Discourse".to_string(),
            last_active: None,
            detail: None,
            error: Some(e.to_string()),
            status: None,
            status_expires: None,
        },
    }
}

async fn check_bodhi(username: &str) -> ServiceLastSeen {
    let client = sandogasa_bodhi::BodhiClient::new();
    let updates = client.updates_for_user(username, 1).await;
    let comments = client.comments_for_user(username, 1).await;

    let update_ts = updates
        .ok()
        .and_then(|u| u.into_iter().next())
        .and_then(|u| u.date_submitted);
    let comment_ts = comments
        .ok()
        .and_then(|c| c.into_iter().next())
        .and_then(|c| c.timestamp);

    // Pick the more recent of update or comment
    let (ts, detail) = match (&update_ts, &comment_ts) {
        (Some(u), Some(c)) if u >= c => (Some(u.clone()), "last update submitted"),
        (Some(_), Some(c)) => (Some(c.clone()), "last comment"),
        (Some(u), None) => (Some(u.clone()), "last update submitted"),
        (None, Some(c)) => (Some(c.clone()), "last comment"),
        (None, None) => (None, ""),
    };

    // Bodhi timestamps are "YYYY-MM-DD HH:MM:SS" UTC — convert to RFC 3339
    let rfc3339 = ts
        .as_deref()
        .and_then(|s| parse_bodhi_timestamp(s).map(|dt| dt.to_rfc3339()));

    ServiceLastSeen {
        service: "Bodhi".to_string(),
        last_active: rfc3339,
        detail: if detail.is_empty() {
            None
        } else {
            Some(detail.to_string())
        },
        error: None,
        status: None,
        status_expires: None,
    }
}

async fn check_distgit(username: &str) -> ServiceLastSeen {
    let client = sandogasa_distgit::DistGitClient::new();
    match client.user_activity_stats(username).await {
        Ok(stats) => {
            let latest = stats.keys().max().cloned();
            ServiceLastSeen {
                service: "Dist-git".to_string(),
                last_active: latest
                    .as_deref()
                    .and_then(|d| {
                        NaiveDateTime::parse_from_str(&format!("{d} 23:59:59"), "%Y-%m-%d %H:%M:%S")
                            .ok()
                    })
                    .map(|naive| naive.and_utc().to_rfc3339()),
                detail: latest.map(|d| format!("last active on {d}")),
                error: None,
                status: None,
                status_expires: None,
            }
        }
        Err(e) => ServiceLastSeen {
            service: "Dist-git".to_string(),
            last_active: None,
            detail: None,
            error: Some(e.to_string()),
            status: None,
            status_expires: None,
        },
    }
}

async fn check_bugzilla(emails: &[String]) -> ServiceLastSeen {
    let client = sandogasa_bugzilla::BzClient::new("https://bugzilla.redhat.com");
    for email in emails {
        let bugs = client
            .search(&format!("creator={email}&limit=1&order=bug_id%20DESC"), 1)
            .await;
        if let Ok(bugs) = bugs
            && let Some(bug) = bugs.into_iter().next()
        {
            return ServiceLastSeen {
                service: "Bugzilla".to_string(),
                last_active: Some(bug.creation_time.to_rfc3339()),
                detail: Some(format!("#{} {}", bug.id, bug.summary)),
                error: None,
                status: None,
                status_expires: None,
            };
        }
    }
    ServiceLastSeen {
        service: "Bugzilla".to_string(),
        last_active: None,
        detail: None,
        error: None,
        status: None,
        status_expires: None,
    }
}

async fn check_mailman(emails: &[String], lists: &[String], max_pages: u32) -> ServiceLastSeen {
    let client = sandogasa_mailman::MailmanClient::new();
    for list in lists {
        if let Ok(Some(id)) = client.find_sender_id(list, emails, max_pages).await
            && let Ok(posts) = client.sender_emails(&id, 1).await
            && let Some(post) = posts.into_iter().next()
        {
            let rfc3339 = post.date.as_deref().and_then(|s| {
                s.parse::<DateTime<chrono::FixedOffset>>()
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
            });
            return ServiceLastSeen {
                service: "Mailing lists".to_string(),
                last_active: rfc3339,
                detail: Some(post.subject),
                error: None,
                status: None,
                status_expires: None,
            };
        }
    }
    ServiceLastSeen {
        service: "Mailing lists".to_string(),
        last_active: None,
        detail: None,
        error: None,
        status: None,
        status_expires: None,
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
    if let Some(ts) = user.last_posted_at {
        println!(
            "  Last post:   {}",
            format_with_relative(&ts.to_rfc3339(), ts)
        );
    }
    if let Some(ts) = user.last_seen_at {
        println!(
            "  Last seen:   {}",
            format_with_relative(&ts.to_rfc3339(), ts)
        );
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
        if let Some(ends) = status.ends_at {
            println!(
                "  Status ends: {}",
                format_with_relative(&ends.to_rfc3339(), ends)
            );
        }
    }

    Ok(())
}

/// Format a `DateTime<Utc>` as a relative time string (e.g. "3 days ago").
fn relative_time(dt: DateTime<Utc>) -> String {
    relative_time_from(dt, Utc::now())
}

/// Format a relative time string given an explicit "now" (for testing).
fn relative_time_from(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let duration = now.signed_duration_since(dt);
    let seconds = duration.num_seconds();
    let (abs_seconds, suffix, prefix) = if seconds < 0 {
        (-seconds, "", "in ")
    } else {
        (seconds, " ago", "")
    };

    let abs_minutes = abs_seconds / 60;
    let abs_hours = abs_seconds / 3600;
    let abs_days = abs_seconds / 86400;
    let abs_weeks = abs_days / 7;
    let abs_months = abs_days / 30;
    let abs_years = abs_days / 365;

    match () {
        _ if abs_seconds < 60 => "just now".to_string(),
        _ if abs_minutes == 1 => format!("{prefix}1 minute{suffix}"),
        _ if abs_minutes < 60 => format!("{prefix}{abs_minutes} minutes{suffix}"),
        _ if abs_hours == 1 => format!("{prefix}1 hour{suffix}"),
        _ if abs_hours < 24 => format!("{prefix}{abs_hours} hours{suffix}"),
        _ if abs_days == 1 => format!("{prefix}1 day{suffix}"),
        _ if abs_days < 7 => format!("{prefix}{abs_days} days{suffix}"),
        _ if abs_weeks == 1 => format!("{prefix}1 week{suffix}"),
        _ if abs_weeks < 5 => format!("{prefix}{abs_weeks} weeks{suffix}"),
        _ if abs_months == 1 => format!("{prefix}1 month{suffix}"),
        _ if abs_months < 12 => format!("{prefix}{abs_months} months{suffix}"),
        _ if abs_years == 1 => format!("{prefix}1 year{suffix}"),
        _ => format!("{prefix}{abs_years} years{suffix}"),
    }
}

/// Parse a Bodhi timestamp ("YYYY-MM-DD HH:MM:SS", assumed UTC) into DateTime.
fn parse_bodhi_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

/// Format a timestamp with relative time appended, e.g. "2026-03-20 23:44:44 (3 days ago)".
fn format_with_relative(ts: &str, dt: DateTime<Utc>) -> String {
    format!("{ts} ({})", relative_time(dt))
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

    // ---- relative_time ----

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse::<DateTime<Utc>>().unwrap()
    }

    #[test]
    fn relative_time_just_now() {
        let now = utc("2026-03-23T12:00:00Z");
        let dt = utc("2026-03-23T11:59:30Z");
        assert_eq!(relative_time_from(dt, now), "just now");
    }

    #[test]
    fn relative_time_minutes() {
        let now = utc("2026-03-23T12:00:00Z");
        assert_eq!(
            relative_time_from(utc("2026-03-23T11:55:00Z"), now),
            "5 minutes ago"
        );
        assert_eq!(
            relative_time_from(utc("2026-03-23T11:59:00Z"), now),
            "1 minute ago"
        );
    }

    #[test]
    fn relative_time_hours() {
        let now = utc("2026-03-23T12:00:00Z");
        assert_eq!(
            relative_time_from(utc("2026-03-23T11:00:00Z"), now),
            "1 hour ago"
        );
        assert_eq!(
            relative_time_from(utc("2026-03-23T06:00:00Z"), now),
            "6 hours ago"
        );
    }

    #[test]
    fn relative_time_days() {
        let now = utc("2026-03-23T12:00:00Z");
        assert_eq!(
            relative_time_from(utc("2026-03-22T12:00:00Z"), now),
            "1 day ago"
        );
        assert_eq!(
            relative_time_from(utc("2026-03-20T12:00:00Z"), now),
            "3 days ago"
        );
    }

    #[test]
    fn relative_time_weeks() {
        let now = utc("2026-03-23T12:00:00Z");
        assert_eq!(
            relative_time_from(utc("2026-03-16T12:00:00Z"), now),
            "1 week ago"
        );
        assert_eq!(
            relative_time_from(utc("2026-03-02T12:00:00Z"), now),
            "3 weeks ago"
        );
    }

    #[test]
    fn relative_time_months() {
        let now = utc("2026-06-23T12:00:00Z");
        // 38 days ago -> past 5 weeks threshold, into months
        assert_eq!(
            relative_time_from(utc("2026-05-16T12:00:00Z"), now),
            "1 month ago"
        );
        assert_eq!(
            relative_time_from(utc("2026-03-23T12:00:00Z"), now),
            "3 months ago"
        );
    }

    #[test]
    fn relative_time_years() {
        let now = utc("2026-03-23T12:00:00Z");
        assert_eq!(
            relative_time_from(utc("2025-03-23T12:00:00Z"), now),
            "1 year ago"
        );
        assert_eq!(
            relative_time_from(utc("2023-03-23T12:00:00Z"), now),
            "3 years ago"
        );
    }

    #[test]
    fn relative_time_future_hours() {
        let now = utc("2026-03-23T12:00:00Z");
        assert_eq!(
            relative_time_from(utc("2026-03-23T14:00:00Z"), now),
            "in 2 hours"
        );
    }

    #[test]
    fn relative_time_future_days() {
        let now = utc("2026-03-23T12:00:00Z");
        assert_eq!(
            relative_time_from(utc("2026-03-24T12:00:00Z"), now),
            "in 1 day"
        );
        assert_eq!(
            relative_time_from(utc("2026-03-26T12:00:00Z"), now),
            "in 3 days"
        );
    }

    // ---- parse_bodhi_timestamp ----

    #[test]
    fn parse_bodhi_timestamp_valid() {
        let dt = parse_bodhi_timestamp("2026-03-20 23:44:44").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-03-20T23:44:44+00:00");
    }

    #[test]
    fn parse_bodhi_timestamp_invalid() {
        assert!(parse_bodhi_timestamp("not a timestamp").is_none());
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

    // ---- extract_list_name ----

    #[test]
    fn extract_list_name_from_api_url() {
        assert_eq!(
            extract_list_name(
                "https://lists.fedoraproject.org/archives/api/list/devel@lists.fedoraproject.org/"
            ),
            "devel@lists.fedoraproject.org"
        );
    }

    #[test]
    fn extract_list_name_with_format_param() {
        assert_eq!(
            extract_list_name(
                "https://example.com/archives/api/list/test@example.com/?format=json"
            ),
            "test@example.com"
        );
    }

    #[test]
    fn extract_list_name_no_pattern() {
        assert_eq!(
            extract_list_name("https://example.com/something"),
            "https://example.com/something"
        );
    }

    // ---- Last-seen serialization ----

    #[test]
    fn last_seen_serializes() {
        let summary = LastSeenSummary {
            username: "salimma".to_string(),
            services: vec![
                ServiceLastSeen {
                    service: "Discourse".to_string(),
                    last_active: Some("2026-03-17T14:50:30+00:00".to_string()),
                    detail: Some("last post".to_string()),
                    error: None,
                    status: Some("🏖️ on vacation".to_string()),
                    status_expires: Some("2026-04-01 00:00 UTC (in 1 week)".to_string()),
                },
                ServiceLastSeen {
                    service: "Bugzilla".to_string(),
                    last_active: None,
                    detail: None,
                    error: Some("no results".to_string()),
                    status: None,
                    status_expires: None,
                },
            ],
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&summary).unwrap()).unwrap();
        assert_eq!(json["username"], "salimma");
        assert_eq!(json["services"][0]["service"], "Discourse");
        assert!(json["services"][0].get("error").is_none());
        assert_eq!(json["services"][1]["error"], "no results");
        assert!(json["services"][1].get("last_active").is_none());
        assert_eq!(json["services"][0]["status"], "🏖️ on vacation");
        assert_eq!(
            json["services"][0]["status_expires"],
            "2026-04-01 00:00 UTC (in 1 week)"
        );
        assert!(json["services"][1].get("status").is_none());
        assert!(json["services"][1].get("status_expires").is_none());
    }

    // ---- Dist-git serialization ----

    #[test]
    fn distgit_activity_serializes_full() {
        let activity = DistgitActivity {
            username: "salimma".to_string(),
            recent_days: vec![
                DistgitDay {
                    date: "2026-03-20".to_string(),
                    actions: 24,
                },
                DistgitDay {
                    date: "2026-03-19".to_string(),
                    actions: 39,
                },
            ],
            last_pr: Some(DistgitPr {
                id: 1,
                title: "Update to 1.0".to_string(),
                status: "Merged".to_string(),
                project: Some("rpms/freerdp".to_string()),
                date_created: Some("2026-03-20T10:00:00+00:00".to_string()),
            }),
            actionable_prs: vec![DistgitPr {
                id: 4,
                title: "Update to 2.1.0".to_string(),
                status: "Open".to_string(),
                project: Some("rpms/python-send2trash".to_string()),
                date_created: None,
            }],
            actionable_total: 3,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert_eq!(json["username"], "salimma");
        assert_eq!(json["recent_days"][0]["date"], "2026-03-20");
        assert_eq!(json["recent_days"][0]["actions"], 24);
        assert_eq!(json["last_pr"]["project"], "rpms/freerdp");
        assert_eq!(json["actionable_total"], 3);
        assert_eq!(
            json["actionable_prs"][0]["project"],
            "rpms/python-send2trash"
        );
    }

    #[test]
    fn distgit_activity_empty() {
        let activity = DistgitActivity {
            username: "nobody".to_string(),
            recent_days: vec![],
            last_pr: None,
            actionable_prs: vec![],
            actionable_total: 0,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert!(json.get("recent_days").is_none());
        assert!(json.get("last_pr").is_none());
    }

    // ---- unix_to_rfc3339 ----

    #[test]
    fn unix_to_rfc3339_valid() {
        assert_eq!(
            unix_to_rfc3339("1773953041").unwrap(),
            "2026-03-19T20:44:01+00:00"
        );
    }

    #[test]
    fn unix_to_rfc3339_invalid() {
        assert!(unix_to_rfc3339("not_a_number").is_none());
    }

    // ---- resolve_emails ----

    #[test]
    fn resolve_emails_with_override() {
        let result = resolve_emails(None, &["test@example.com".to_string()], false).unwrap();
        assert_eq!(result, vec!["test@example.com"]);
    }

    #[test]
    fn resolve_emails_multiple_overrides() {
        let overrides = vec!["a@x.com".to_string(), "b@y.com".to_string()];
        let result = resolve_emails(None, &overrides, false).unwrap();
        assert_eq!(result, vec!["a@x.com", "b@y.com"]);
    }

    #[test]
    fn resolve_emails_no_fas_uses_fedoraproject_org() {
        let result = resolve_emails(Some("salimma"), &[], true).unwrap();
        assert_eq!(result, vec!["salimma@fedoraproject.org"]);
    }

    #[test]
    fn resolve_emails_no_username_no_email_errors() {
        let result = resolve_emails(None, &[], false);
        assert!(result.is_err());
    }

    // ---- format_with_relative ----

    #[test]
    fn format_with_relative_includes_timestamp_and_relative() {
        let dt = Utc::now();
        let ts = dt.to_rfc3339();
        let formatted = format_with_relative(&ts, dt);
        assert!(formatted.starts_with(&ts));
        assert!(formatted.contains("just now"));
    }

    // ---- Bugzilla serialization ----

    #[test]
    fn bugzilla_activity_serializes_full() {
        let activity = BugzillaActivity {
            emails_searched: vec!["salimma@fedoraproject.org".to_string()],
            last_filed: Some(BugSummary {
                id: 2342424,
                summary: "freerdp needs update".to_string(),
                status: "NEW".to_string(),
                component: Some("freerdp".to_string()),
                date: Some("2026-03-15T10:00:00+00:00".to_string()),
            }),
            last_changed: Some(BugSummary {
                id: 2340000,
                summary: "CVE-2026-1234 libxml2".to_string(),
                status: "ASSIGNED".to_string(),
                component: Some("libxml2".to_string()),
                date: Some("2026-03-20T14:30:00+00:00".to_string()),
            }),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert_eq!(json["last_filed"]["id"], 2342424);
        assert_eq!(json["last_filed"]["status"], "NEW");
        assert_eq!(json["last_changed"]["id"], 2340000);
        assert_eq!(json["last_changed"]["component"], "libxml2");
    }

    #[test]
    fn bugzilla_activity_no_results() {
        let activity = BugzillaActivity {
            emails_searched: vec!["nobody@example.com".to_string()],
            last_filed: None,
            last_changed: None,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert!(json.get("last_filed").is_none());
        assert!(json.get("last_changed").is_none());
    }

    // ---- Mailman serialization ----

    #[test]
    fn mailman_activity_serializes_full() {
        let activity = MailmanActivity {
            username: Some("salimma".to_string()),
            emails_searched: vec![
                "salimma@fedoraproject.org".to_string(),
                "michel@michel-slm.name".to_string(),
            ],
            mailman_id: Some("abc123".to_string()),
            recent_posts: vec![MailmanPost {
                subject: "Re: Test subject".to_string(),
                list: Some("devel@lists.fedoraproject.org".to_string()),
                date: Some("2026-03-23T12:00:00+00:00".to_string()),
            }],
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert_eq!(json["emails_searched"][0], "salimma@fedoraproject.org");
        assert_eq!(json["emails_searched"][1], "michel@michel-slm.name");
        assert_eq!(json["mailman_id"], "abc123");
        assert_eq!(json["recent_posts"][0]["subject"], "Re: Test subject");
        assert_eq!(
            json["recent_posts"][0]["list"],
            "devel@lists.fedoraproject.org"
        );
    }

    #[test]
    fn mailman_activity_no_posts() {
        let activity = MailmanActivity {
            username: None,
            emails_searched: vec!["user@example.com".to_string()],
            mailman_id: None,
            recent_posts: vec![],
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&activity).unwrap()).unwrap();
        assert!(json.get("username").is_none());
        assert!(json.get("mailman_id").is_none());
        assert_eq!(json["recent_posts"].as_array().unwrap().len(), 0);
    }
}
