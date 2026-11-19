// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Adding entries to Fedora Calendar as a logged-in user.
//!
//! fedocal has no API for writing, and logs users in through
//! id.fedoraproject.org's OpenID Connect provider, which takes a Kerberos
//! ticket as well as a password. [`Session::kerberos`] follows that login
//! with curl, which answers the provider's `Negotiate` challenge with the
//! user's ticket (from `fkinit`), and keeps fedocal's session cookie;
//! [`Session::add`] then fills in and submits the add form, CSRF token
//! included, as the browser would.
//!
//! curl must be told to send the ticket across the redirect from fedocal
//! to id.fedoraproject.org (`--location-trusted`); it does not otherwise
//! authenticate to a host other than the one first asked. fedocal's
//! session lasts an hour, so a session is for one run.

use std::sync::{Arc, Mutex};

use chrono::NaiveDate;

use crate::Error;

/// An entry to add.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NewEntry {
    pub calendar: String,
    pub name: String,
    pub first_day: NaiveDate,
    /// The last day it takes (fedocal stores the day after).
    pub last_day: NaiveDate,
    pub information: String,
    pub location: String,
}

/// What to change in an existing entry; what is left `None` stays as
/// it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Changes {
    pub name: Option<String>,
    /// The first and last day it takes.
    pub days: Option<(NaiveDate, NaiveDate)>,
    pub information: Option<String>,
    pub location: Option<String>,
    /// For a recurring entry, the last day the series may repeat on.
    pub until: Option<NaiveDate>,
    /// For a recurring entry, change the series from the occurrence the
    /// form shows (the next one) on, fedocal's "Save all", rather than
    /// that occurrence alone ("Save"). fedocal keeps the earlier
    /// occurrences as a separate entry.
    pub whole_series: bool,
}

/// An entry's edit form, as a browser would send it, and the save
/// buttons it offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EntryForm {
    pub fields: Vec<(String, String)>,
    /// The submit buttons' values: `Save`, and for a recurring entry
    /// `Save all`.
    pub submits: Vec<String>,
}

/// A logged-in session on a Fedora Calendar instance.
#[derive(Debug, Clone)]
pub struct Session {
    http: reqwest::Client,
    base_url: String,
    /// The cookies for the host, as `(name, value)`: those the login
    /// left (fedocal's session, named by its deployment, and the load
    /// balancer's), updated from every answer as a browser does. Opening
    /// a form stores its CSRF secret in a new session cookie, which the
    /// form's submission must carry.
    cookies: Arc<Mutex<Vec<(String, String)>>>,
}

impl NewEntry {
    /// A full-day entry in `calendar` from `first_day` to `last_day`.
    pub fn full_days(
        calendar: impl Into<String>,
        name: impl Into<String>,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Self {
        Self {
            calendar: calendar.into(),
            name: name.into(),
            first_day,
            last_day,
            information: String::new(),
            location: String::new(),
        }
    }

    pub fn information(mut self, information: impl Into<String>) -> Self {
        self.information = information.into();
        self
    }

    /// The add form's fields: a full-day entry is sent from 00:00 to 00:00
    /// UTC, as fedocal's form still asks for times and a time zone.
    pub fn form(&self, csrf_token: &str) -> Vec<(&'static str, String)> {
        vec![
            ("csrf_token", csrf_token.to_string()),
            ("calendar_name", self.calendar.clone()),
            ("meeting_name", self.name.clone()),
            ("meeting_date", self.first_day.to_string()),
            ("meeting_date_end", self.last_day.to_string()),
            ("meeting_time_start", "00:00".to_string()),
            ("meeting_time_stop", "00:00".to_string()),
            ("meeting_timezone", "UTC".to_string()),
            ("full_day", "y".to_string()),
            ("information", self.information.clone()),
            ("meeting_location", self.location.clone()),
        ]
    }
}

impl Changes {
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn days(mut self, first: NaiveDate, last: NaiveDate) -> Self {
        self.days = Some((first, last));
        self
    }

    pub fn information(mut self, information: impl Into<String>) -> Self {
        self.information = Some(information.into());
        self
    }

    pub fn location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn until(mut self, until: NaiveDate) -> Self {
        self.until = Some(until);
        self
    }

    pub fn whole_series(mut self, whole: bool) -> Self {
        self.whole_series = whole;
        self
    }

    /// Whether it changes nothing.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.days.is_none()
            && self.information.is_none()
            && self.location.is_none()
            && self.until.is_none()
    }

    /// `fields` (an edit form's, as a browser would send them) with these
    /// changes made. Saving an edit stores the end date as given, so the
    /// day after the last is sent, which is how fedocal stores a full-day
    /// entry's end, as the form itself shows it.
    fn apply(&self, fields: &mut [(String, String)]) {
        let mut set = |name: &str, value: String| {
            if let Some(field) = fields.iter_mut().find(|(n, _)| n == name) {
                field.1 = value;
            }
        };
        if let Some(name) = &self.name {
            set("meeting_name", name.clone());
        }
        if let Some((first, last)) = self.days {
            set("meeting_date", first.to_string());
            let end = last.succ_opt().unwrap_or(last);
            set("meeting_date_end", end.to_string());
        }
        if let Some(information) = &self.information {
            set("information", information.clone());
        }
        if let Some(location) = &self.location {
            set("meeting_location", location.clone());
        }
        if let Some(until) = self.until {
            set("end_repeats", until.to_string());
        }
    }
}

impl EntryForm {
    /// The value of field `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the entry repeats: its form offers "Save all".
    pub fn recurring(&self) -> bool {
        self.submits.iter().any(|v| v == "Save all")
    }
}

impl Session {
    /// Log in to the instance at `base_url` with the user's Kerberos
    /// ticket. Blocking: it runs curl, about a second. Checks first that
    /// curl and a ticket are there, and says how to get a ticket if not.
    pub fn kerberos(base_url: &str) -> Result<Session, Error> {
        sandogasa_cli::require_tools(&[("curl", "sudo dnf install curl", Some("--version"))])
            .map_err(Error::Login)?;
        let ticket = std::process::Command::new("klist")
            .arg("-s")
            .status()
            .map_err(|e| {
                Error::Login(format!(
                    "cannot run klist ({e}); install krb5-workstation and run \
                     `fkinit -u <FAS username>`"
                ))
            })?;
        if !ticket.success() {
            return Err(Error::Login(
                "no Kerberos ticket; run `fkinit -u <FAS username>`".into(),
            ));
        }
        let attempt = || {
            Session::login_with(base_url, |argv| {
                let out = std::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .output()
                    .map_err(|e| format!("cannot run curl: {e}"))?;
                if out.status.success() {
                    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
                } else {
                    Err(format!(
                        "curl failed ({}): {}",
                        out.status,
                        String::from_utf8_lossy(&out.stderr).trim()
                    ))
                }
            })
        };
        retried(attempt)
    }

    /// [`Session::kerberos`] with the curl run given: `run` takes the
    /// command line [`login_command`] makes and returns what curl printed
    /// (the URL it ended at), the cookie jar being read afterwards.
    pub fn login_with(
        base_url: &str,
        run: impl FnOnce(&[String]) -> Result<String, String>,
    ) -> Result<Session, Error> {
        let base_url = base_url.trim_end_matches('/');
        let jar = tempfile::NamedTempFile::new()
            .map_err(|e| Error::Login(format!("cannot make a cookie jar: {e}")))?;
        let argv = login_command(base_url, &jar.path().display().to_string());
        let ended_at = run(&argv).map_err(Error::Login)?;
        let jar_text = std::fs::read_to_string(jar.path())
            .map_err(|e| Error::Login(format!("cannot read the cookie jar: {e}")))?;
        let host = host_of(base_url);
        // A login the ticket did not get through ends on
        // id.fedoraproject.org's password form instead.
        let cookies = host_cookies(&jar_text, host);
        if cookies.is_empty() || host_of(&ended_at) != host {
            return Err(Error::Login(format!(
                "{TURNED_DOWN} {} without a session; is the Kerberos ticket \
                 (`klist`) for your FAS account?",
                if ended_at.is_empty() {
                    "no page"
                } else {
                    ended_at.trim()
                }
            )));
        }
        Ok(Session {
            http: no_redirect_client(),
            base_url: base_url.to_string(),
            cookies: Arc::new(Mutex::new(cookies)),
        })
    }

    /// Whether the user may add to `calendar`: its add form opens, with a
    /// CSRF token. Nothing is submitted; a refusal carries fedocal's
    /// reasons.
    pub async fn can_add(&self, calendar: &str) -> Result<(), Error> {
        self.form_token(calendar).await.map(|_| ())
    }

    /// Add `entry`: read the add form for its CSRF token, then submit it.
    /// fedocal answers a success with a redirect to the calendar; a
    /// refusal shows the form again, with its reasons, which are
    /// returned as [`Error::Refused`].
    pub async fn add(&self, entry: &NewEntry) -> Result<(), Error> {
        let url = format!("{}/{}/add/", self.base_url, entry.calendar);
        let token = self.form_token(&entry.calendar).await?;
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .form(&entry.form(&token))
            .send()
            .await
            .map_err(|e| Error::Http(format!("POST {url}: {e}")))?;
        self.keep_cookies(&resp);
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .unwrap_or_default()
                .to_string();
            // fedocal shows a filed entry's day in the calendar; any
            // other redirect (the calendar itself when the user may not
            // add, the front page when the session expired) is a refusal,
            // with its reasons on the page it leads to.
            if is_filed_redirect(&location, &self.base_url, &entry.calendar) {
                return Ok(());
            }
            let page = self
                .page(&self.absolute(&location))
                .await
                .unwrap_or_default();
            let mut reasons = page_messages(&page);
            if reasons.is_empty() {
                reasons.push(format!("the form led to {location}, not the entry's day"));
            }
            return Err(Error::Refused(reasons));
        }
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return Err(Error::Refused(page_messages(&body)));
        }
        Err(Error::Http(format!("POST {url}: HTTP {status}")))
    }

    /// Whether the user may delete entry `meeting_id`: its delete form
    /// opens. Nothing is submitted.
    pub async fn can_cancel(&self, meeting_id: u64) -> Result<(), Error> {
        self.delete_token(meeting_id, None).await.map(|_| ())
    }

    /// Cancel the occurrence of entry `meeting_id` on `date` (the
    /// meeting's own date, in its time zone), or the entry itself when it
    /// does not repeat, through fedocal's delete form. For a recurring
    /// entry fedocal ends the series the day before and starts it again,
    /// as a new entry, after that occurrence. fedocal answers with the
    /// same redirect whether or not anything was deleted, so the caller
    /// reads the calendar back.
    pub async fn cancel(&self, meeting_id: u64, date: NaiveDate) -> Result<(), Error> {
        let url = format!("{}/meeting/delete/{meeting_id}/", self.base_url);
        let token = self.delete_token(meeting_id, Some(date)).await?;
        let fields = [
            ("csrf_token", token),
            ("confirm_delete", "y".to_string()),
            ("from_date", date.to_string()),
        ];
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .form(&fields)
            .send()
            .await
            .map_err(|e| Error::Http(format!("POST {url}: {e}")))?;
        self.keep_cookies(&resp);
        let status = resp.status();
        if status.is_redirection() {
            // fedocal redirects when it refuses too (not a manager, a
            // calendar that is not enabled), with the reason flashed on
            // the page it leads to.
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let page = self
                .page(&self.absolute(&location))
                .await
                .unwrap_or_default();
            let reasons = page_messages(&page);
            if reasons.is_empty() {
                return Ok(());
            }
            return Err(Error::Refused(reasons));
        }
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return Err(Error::Refused(page_messages(&body)));
        }
        Err(Error::Http(format!("POST {url}: HTTP {status}")))
    }

    /// Entry `meeting_id` as its edit form shows it (for a recurring
    /// entry, from its next occurrence), with the save buttons offered.
    pub async fn entry(&self, meeting_id: u64) -> Result<EntryForm, Error> {
        self.edit_form(meeting_id).await
    }

    /// Whether the user may edit entry `meeting_id`: its edit form opens.
    /// Nothing is submitted; a refusal carries fedocal's reasons.
    pub async fn can_edit(&self, meeting_id: u64) -> Result<(), Error> {
        self.edit_form(meeting_id).await.map(|_| ())
    }

    /// Change entry `meeting_id`: read its edit form as the browser would
    /// send it, make `changes`, and submit it, so the fields the API does
    /// not show (co-managers, reminders) are kept. fedocal answers a
    /// saved edit with a redirect to the entry's page; anything else is a
    /// refusal, with fedocal's reasons.
    pub async fn edit(&self, meeting_id: u64, changes: &Changes) -> Result<(), Error> {
        let url = format!("{}/meeting/edit/{meeting_id}/", self.base_url);
        let form = self.edit_form(meeting_id).await?;
        let recurring = form.recurring();
        let mut fields = form.fields;
        changes.apply(&mut fields);
        // The button pressed decides what a recurring entry's edit
        // covers; a single entry has only "Save".
        let action = if changes.whole_series && recurring {
            "Save all"
        } else {
            "Save"
        };
        if let Some(field) = fields.iter_mut().find(|(n, _)| n == "action") {
            field.1 = action.to_string();
        }
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .form(&fields)
            .send()
            .await
            .map_err(|e| Error::Http(format!("POST {url}: {e}")))?;
        self.keep_cookies(&resp);
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if path_of(&self.absolute(&location), &self.base_url)
                == Some(format!("/meeting/{meeting_id}/"))
            {
                return Ok(());
            }
            let page = self
                .page(&self.absolute(&location))
                .await
                .unwrap_or_default();
            let mut reasons = page_messages(&page);
            if reasons.is_empty() {
                reasons.push(format!("the form led to {location}, not the entry"));
            }
            return Err(Error::Refused(reasons));
        }
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return Err(Error::Refused(page_messages(&body)));
        }
        Err(Error::Http(format!("POST {url}: HTTP {status}")))
    }

    /// Entry `meeting_id`'s edit form, as a browser would send it; when it
    /// does not open, fedocal's reasons.
    async fn edit_form(&self, meeting_id: u64) -> Result<EntryForm, Error> {
        let url = format!("{}/meeting/edit/{meeting_id}/", self.base_url);
        let page = self.page(&url).await?;
        let submits = submit_values(&page);
        form_values(&page)
            .map(|fields| EntryForm { fields, submits })
            .ok_or_else(|| {
                let reasons = page_messages(&page);
                if reasons.is_empty() {
                    Error::Http(format!(
                        "no edit form on {url}; is the session still valid?"
                    ))
                } else {
                    Error::Refused(reasons)
                }
            })
    }

    /// The CSRF token of entry `meeting_id`'s delete form (at `date`); when
    /// the form does not open, fedocal's reasons.
    async fn delete_token(
        &self,
        meeting_id: u64,
        date: Option<NaiveDate>,
    ) -> Result<String, Error> {
        let mut url = format!("{}/meeting/delete/{meeting_id}/", self.base_url);
        if let Some(date) = date {
            url.push_str(&format!("?from_date={date}"));
        }
        let page = self.page(&url).await?;
        csrf_token(&page).ok_or_else(|| {
            let reasons = page_messages(&page);
            if reasons.is_empty() {
                Error::Http(format!(
                    "no delete form on {url}; is the session still valid?"
                ))
            } else {
                Error::Refused(reasons)
            }
        })
    }

    /// The CSRF token of `calendar`'s add form; when the form does not
    /// open, fedocal's reasons.
    async fn form_token(&self, calendar: &str) -> Result<String, Error> {
        let url = format!("{}/{calendar}/add/", self.base_url);
        let page = self.page(&url).await?;
        csrf_token(&page).ok_or_else(|| {
            let reasons = page_messages(&page);
            if reasons.is_empty() {
                Error::Http(format!(
                    "no CSRF token on {url}; is the session still valid?"
                ))
            } else {
                Error::Refused(reasons)
            }
        })
    }

    /// A page, as the logged-in user. A redirect away from it (to the
    /// calendar, when the user may not add to it) is followed once, for
    /// its messages.
    async fn page(&self, url: &str) -> Result<String, Error> {
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .send()
            .await
            .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;
        self.keep_cookies(&resp);
        if resp.status().is_redirection() {
            let next = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .map(|l| self.absolute(l))
                .unwrap_or_default();
            if next.is_empty() || host_of(&next) != host_of(&self.base_url) {
                return Err(Error::Http(format!("GET {url}: redirected to {next:?}")));
            }
            let resp = self
                .http
                .get(&next)
                .header(reqwest::header::COOKIE, self.cookie_header())
                .send()
                .await
                .map_err(|e| Error::Http(format!("GET {next}: {e}")))?;
            self.keep_cookies(&resp);
            return resp
                .text()
                .await
                .map_err(|e| Error::Http(format!("GET {next}: {e}")));
        }
        let resp = sandogasa_cli::http::ok(resp, &format!("GET {url}"))
            .await
            .map_err(Error::Http)?;
        resp.text()
            .await
            .map_err(|e| Error::Http(format!("GET {url}: {e}")))
    }

    /// `location` (a redirect's) as an absolute URL on this instance.
    fn absolute(&self, location: &str) -> String {
        if location.starts_with('/') {
            format!("{}{location}", self.base_url)
        } else {
            location.to_string()
        }
    }

    fn cookie_header(&self) -> String {
        let cookies = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
        cookies
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Take in the cookies an answer sets, replacing those of the same
    /// name.
    fn keep_cookies(&self, resp: &reqwest::Response) {
        let mut cookies = self.cookies.lock().unwrap_or_else(|e| e.into_inner());
        for header in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            let Some((name, value)) = header
                .to_str()
                .ok()
                .and_then(|h| h.split(';').next())
                .and_then(|pair| pair.split_once('='))
            else {
                continue;
            };
            let (name, value) = (name.trim().to_string(), value.trim().to_string());
            match cookies.iter_mut().find(|(n, _)| *n == name) {
                Some(cookie) => cookie.1 = value,
                None => cookies.push((name, value)),
            }
        }
    }
}

/// How a login the provider turned down is reported (it ended on its
/// page rather than the calendar's).
const TURNED_DOWN: &str = "the login ended at";

/// `attempt`, and once more when the provider turned the ticket down:
/// id.fedoraproject.org now and then turns down a ticket it takes on the
/// next try.
fn retried(attempt: impl Fn() -> Result<Session, Error>) -> Result<Session, Error> {
    match attempt() {
        Err(Error::Login(why)) if why.starts_with(TURNED_DOWN) => attempt(),
        done => done,
    }
}

/// The curl command that logs in to `base_url` with the Kerberos ticket
/// and keeps the cookies in `jar`, printing the URL it ends at. HTTPS
/// only, unless `base_url` itself is not (a test server).
pub fn login_command(base_url: &str, jar: &str) -> Vec<String> {
    let mut argv: Vec<String> = [
        "curl",
        "--silent",
        "--show-error",
        "--location",
        // Send the ticket to id.fedoraproject.org too, after fedocal's
        // redirect there.
        "--location-trusted",
        "--negotiate",
        "--user",
        ":",
        "--max-redirs",
        "15",
        "--max-time",
        "60",
        "--user-agent",
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        "--cookie-jar",
        jar,
        "--cookie",
        jar,
        "--output",
        "/dev/null",
        "--write-out",
        "%{url_effective}",
    ]
    .map(String::from)
    .into();
    if base_url.starts_with("https://") {
        argv.extend(["--proto".to_string(), "=https".to_string()]);
    }
    argv.push(format!("{base_url}/login/"));
    argv
}

/// The cookies for `host` in a Netscape-format cookie jar, as curl writes
/// one, as `(name, value)`.
fn host_cookies(jar: &str, host: &str) -> Vec<(String, String)> {
    jar.lines()
        .filter_map(|line| {
            // curl marks HttpOnly cookies with this prefix.
            let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if line.starts_with('#') {
                return None;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let [domain, _, _, _, _, name, value] = fields[..] else {
                return None;
            };
            (domain.trim_start_matches('.') == host && !name.is_empty())
                .then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

/// Whether a redirect after submitting leads to a day in `calendar`
/// (`/<calendar>/<year>/<month>/<day>/`) on the instance at `base_url`,
/// which is where fedocal shows an entry it filed.
fn is_filed_redirect(location: &str, base_url: &str, calendar: &str) -> bool {
    let path = if location.starts_with('/') {
        location
    } else if host_of(location) == host_of(base_url) {
        let rest = location.split_once("://").map_or(location, |(_, r)| r);
        rest.find('/').map_or("/", |at| &rest[at..])
    } else {
        return false;
    };
    let Some(rest) = path.strip_prefix(&format!("/{calendar}/")) else {
        return false;
    };
    let year = rest.split('/').next().unwrap_or("");
    year.len() == 4 && year.chars().all(|c| c.is_ascii_digit())
}

/// The host of a URL, without scheme, port or path.
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.split(':').next().unwrap_or("")
}

/// The path of `url` when it is on the instance at `base_url`.
fn path_of(url: &str, base_url: &str) -> Option<String> {
    if host_of(url) != host_of(base_url) {
        return None;
    }
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    Some(rest.find('/').map_or("/".to_string(), |at| {
        rest[at..]
            .split(['?', '#'])
            .next()
            .unwrap_or("/")
            .to_string()
    }))
}

/// The fields of the page's form that carries a CSRF token, as a browser
/// sends them: text and hidden inputs, checked boxes, the first submit
/// button, textareas, and each select's selected option (or its first).
fn form_values(page: &str) -> Option<Vec<(String, String)>> {
    let token = page.find("name=\"csrf_token\"")?;
    let start = page[..token].rfind("<form")?;
    let end = token + page[token..].find("</form>").unwrap_or(page.len() - token);
    let form = &page[start..end];
    let mut out = Vec::new();
    let mut submitted = false;
    let mut rest = form;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        let (kind, attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let attrs = parse_attrs(attrs);
        let get = |name: &str| {
            attrs
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone().unwrap_or_default())
        };
        let Some(name) = get("name").filter(|n| !n.is_empty()) else {
            continue;
        };
        match kind.to_ascii_lowercase().as_str() {
            "input" => {
                let kind = get("type").unwrap_or_default().to_ascii_lowercase();
                match kind.as_str() {
                    "button" | "reset" | "image" | "file" => {}
                    "submit" => {
                        if !submitted {
                            submitted = true;
                            out.push((name, get("value").unwrap_or_default()));
                        }
                    }
                    "checkbox" | "radio" => {
                        if get("checked").is_some() {
                            out.push((name, get("value").unwrap_or_else(|| "on".into())));
                        }
                    }
                    _ => out.push((name, get("value").unwrap_or_default())),
                }
            }
            "textarea" => {
                let body = &rest[tag_end.min(rest.len())..];
                let text = body.get(1..).unwrap_or("");
                let text = text.split("</textarea>").next().unwrap_or("");
                // A browser drops the line break right after the tag.
                let text = text
                    .strip_prefix("\r\n")
                    .or_else(|| text.strip_prefix('\n'))
                    .unwrap_or(text);
                out.push((name, unescape(text)));
            }
            "select" => {
                let body = rest[tag_end.min(rest.len())..]
                    .split("</select>")
                    .next()
                    .unwrap_or("");
                // Each option's attributes and text; a browser sends the
                // text when there is no value.
                let options: Vec<(Attrs, String)> = body
                    .split("<option")
                    .skip(1)
                    .map(|o| {
                        let (attrs, text) = o.split_once('>').unwrap_or((o, ""));
                        let text = text.split("</option>").next().unwrap_or("");
                        (parse_attrs(attrs), unescape(text.trim()))
                    })
                    .collect();
                let value_of = |(attrs, text): &(Attrs, String)| {
                    attrs
                        .iter()
                        .find(|(n, _)| n == "value")
                        .and_then(|(_, v)| v.clone())
                        .unwrap_or_else(|| text.clone())
                };
                let chosen = options
                    .iter()
                    .find(|(attrs, _)| attrs.iter().any(|(n, _)| n == "selected"))
                    .or(options.first())
                    .map(value_of)
                    .unwrap_or_default();
                out.push((name, chosen));
            }
            _ => {}
        }
    }
    Some(out)
}

/// The values of the submit buttons of the page's form that carries a
/// CSRF token.
fn submit_values(page: &str) -> Vec<String> {
    let Some(token) = page.find("name=\"csrf_token\"") else {
        return Vec::new();
    };
    let Some(start) = page[..token].rfind("<form") else {
        return Vec::new();
    };
    let end = token + page[token..].find("</form>").unwrap_or(page.len() - token);
    page[start..end]
        .split("<input")
        .skip(1)
        .map(|tag| parse_attrs(tag.split('>').next().unwrap_or("")))
        .filter(|attrs| {
            attrs.iter().any(|(n, v)| {
                n == "type"
                    && v.as_deref()
                        .is_some_and(|v| v.eq_ignore_ascii_case("submit"))
            })
        })
        .filter_map(|attrs| {
            attrs
                .into_iter()
                .find(|(n, _)| n == "value")
                .and_then(|(_, v)| v)
        })
        .collect()
}

/// A tag's attributes, by lowercase name; `None` for one without a
/// value.
type Attrs = Vec<(String, Option<String>)>;

/// A tag's attributes: `name`, `name=value`, `name="value"`, values
/// unescaped.
fn parse_attrs(s: &str) -> Attrs {
    let mut out = Vec::new();
    // A self-closing tag's slash, not one that ends an unquoted value.
    let s = s.trim_end();
    let s = match s.strip_suffix('/') {
        Some(rest) if rest.is_empty() || rest.ends_with(char::is_whitespace) => rest,
        _ => s,
    };
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut name = String::new();
        while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '=') {
            name.push(c);
        }
        if name.is_empty() {
            break;
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let value = if chars.next_if_eq(&'=').is_some() {
            while chars.next_if(|c| c.is_whitespace()).is_some() {}
            let mut value = String::new();
            match chars.next_if(|c| *c == '"' || *c == '\'') {
                Some(quote) => {
                    for c in chars.by_ref() {
                        if c == quote {
                            break;
                        }
                        value.push(c);
                    }
                }
                None => {
                    while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                        value.push(c);
                    }
                }
            }
            Some(unescape(&value))
        } else {
            None
        };
        out.push((name.to_ascii_lowercase(), value));
    }
    out
}

/// HTML text with the entities fedocal's pages use replaced.
fn unescape(text: &str) -> String {
    text.replace("&#39;", "'")
        .replace("&#34;", "\"")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The CSRF token of the first form on a page.
fn csrf_token(page: &str) -> Option<String> {
    let at = page.find("name=\"csrf_token\"")?;
    let start = page[..at].rfind('<')?;
    let end = at + page[at..].find('>')?;
    let tag = &page[start..end];
    let value = tag.split_once("value=\"")?.1;
    Some(value.split('"').next()?.to_string()).filter(|v| !v.is_empty())
}

/// The messages a fedocal page shows: flashed errors and warnings, and
/// the errors listed under form fields.
fn page_messages(page: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in [
        "<li class=\"errors\">",
        "<li class=\"warnings\">",
        "<li class=\"error\">",
    ] {
        let mut rest = page;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let text = rest.split("</li>").next().unwrap_or("");
            push_text(&mut out, text);
        }
    }
    // Field errors in a table row, one cell each.
    let mut rest = page;
    while let Some(at) = rest.find("<tr class=\"errors\">") {
        rest = &rest[at..];
        let row = rest.split("</tr>").next().unwrap_or("");
        for cell in row.split("<td>").skip(1) {
            push_text(&mut out, cell.split("</td>").next().unwrap_or(""));
        }
        rest = &rest[row.len()..];
    }
    let mut rest = page;
    while let Some(at) = rest.find("<ul class=\"errors\">") {
        rest = &rest[at..];
        let list = rest.split("</ul>").next().unwrap_or("");
        for item in list.split("<li>").skip(1) {
            push_text(&mut out, item.split("</li>").next().unwrap_or(""));
        }
        rest = &rest[list.len()..];
    }
    out
}

/// `html` as plain text, added to `out` when not empty or already there.
fn push_text(out: &mut Vec<String>, html: &str) {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = unescape(&text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.is_empty() && !out.contains(&text) {
        out.push(text);
    }
}

/// A client that does not follow redirects, so a submission's answer
/// can be read.
fn no_redirect_client() -> reqwest::Client {
    sandogasa_cli::http::builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .redirect(reqwest::redirect::Policy::none())
    .build()
    .expect("build reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// A login whose curl run writes `jar` and ends at `ended_at`.
    fn login(base: &str, jar: &str, ended_at: &str) -> Result<Session, Error> {
        let jar = jar.to_string();
        let ended_at = ended_at.to_string();
        Session::login_with(base, move |argv| {
            let path = &argv[argv.iter().position(|a| a == "--cookie-jar").unwrap() + 1];
            std::fs::write(path, jar).unwrap();
            Ok(ended_at)
        })
    }

    const JAR: &str = "# Netscape HTTP Cookie File\n\
        #HttpOnly_id.fedoraproject.org\tFALSE\t/\tTRUE\t0\tfedora_ipsilon_session_id\tx\n\
        #HttpOnly_calendar.fedoraproject.org\tFALSE\t/\tTRUE\t1795110000\tfedocal\tabc.def\n\
        calendar.fedoraproject.org\tFALSE\t/\tTRUE\t0\tfc3e\t9\n";

    #[test]
    fn the_login_command_sends_the_ticket_across_the_redirect() {
        let argv = login_command("https://calendar.fedoraproject.org", "/tmp/jar");
        assert_eq!(argv[0], "curl");
        for flag in ["--location-trusted", "--negotiate"] {
            assert!(argv.iter().any(|a| a == flag), "{flag}");
        }
        assert!(argv.windows(2).any(|w| w == ["--proto", "=https"]));
        assert_eq!(
            argv.last().unwrap(),
            "https://calendar.fedoraproject.org/login/"
        );
        // A test server over plain HTTP is not limited to HTTPS.
        let local = login_command("http://127.0.0.1:1", "/tmp/jar");
        assert!(!local.iter().any(|a| a == "--proto"));
    }

    #[test]
    fn a_login_keeps_the_calendars_session_cookie() {
        let s = login(
            "https://calendar.fedoraproject.org/",
            JAR,
            "https://calendar.fedoraproject.org/",
        )
        .unwrap();
        // Every cookie for the calendar's host, none for the provider's.
        assert_eq!(s.cookie_header(), "fedocal=abc.def; fc3e=9");
        assert_eq!(s.base_url, "https://calendar.fedoraproject.org");
        // Ending on id.fedoraproject.org's password form: refused.
        let err = login(
            "https://calendar.fedoraproject.org",
            JAR,
            "https://id.fedoraproject.org/login/gssapi/negotiate?x=1",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("ended at https://id.fedoraproject.org"),
            "{err}"
        );
        // No session cookie: refused, and a failed curl is reported.
        assert!(login("https://calendar.fedoraproject.org", "", "").is_err());
        let failed =
            Session::login_with("https://c.example", |_| Err("curl failed".into())).unwrap_err();
        assert_eq!(
            failed.to_string(),
            "cannot log in to Fedora Calendar: curl failed"
        );
    }

    #[test]
    fn a_turned_down_login_is_tried_once_more() {
        let tries = std::cell::Cell::new(0);
        let attempt = || {
            tries.set(tries.get() + 1);
            if tries.get() == 1 {
                login(
                    "https://c.example",
                    JAR,
                    "https://id.fedoraproject.org/login/x",
                )
            } else {
                login(
                    "https://calendar.fedoraproject.org",
                    JAR,
                    "https://calendar.fedoraproject.org/",
                )
            }
        };
        assert!(retried(attempt).is_ok());
        assert_eq!(tries.get(), 2);
        // Another failure is not retried.
        let tries = std::cell::Cell::new(0);
        let failing = || {
            tries.set(tries.get() + 1);
            Session::login_with("https://c.example", |_| Err("curl failed".into()))
        };
        assert!(retried(failing).is_err());
        assert_eq!(tries.get(), 1);
    }

    #[test]
    fn pages_give_their_token_and_messages() {
        let form = r#"<form method="post"><input id="csrf_token" name="csrf_token" type="hidden" value="tok-1"></form>"#;
        assert_eq!(csrf_token(form).as_deref(), Some("tok-1"));
        assert_eq!(csrf_token("<form></form>"), None);
        let refused = r#"<ul id="flashes"><li class="warnings">You are not one of the editors &amp; admins</li></ul>
            <td><input name="meeting_time_start"><ul class="errors"><li>Time must be of type "HH:MM"</li></ul></td>
            <ul class="errors"><li>This field is required.</li><li>This field is required.</li></ul>"#;
        assert_eq!(
            page_messages(refused),
            [
                "You are not one of the editors & admins",
                "Time must be of type \"HH:MM\"",
                "This field is required."
            ]
        );
        assert_eq!(host_of("http://127.0.0.1:8080/x?y"), "127.0.0.1");
        assert_eq!(
            host_of("calendar.fedoraproject.org"),
            "calendar.fedoraproject.org"
        );
    }

    #[test]
    fn a_full_day_entry_is_sent_from_midnight_to_midnight_utc() {
        let entry = NewEntry::full_days("vacation", "PTO", day("2026-12-21"), day("2027-01-04"))
            .information("Back on the 5th");
        let form = entry.form("tok");
        let get = |k: &str| form.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        assert_eq!(get("csrf_token"), "tok");
        assert_eq!(get("meeting_date"), "2026-12-21");
        assert_eq!(get("meeting_date_end"), "2027-01-04");
        assert_eq!(get("meeting_time_start"), "00:00");
        assert_eq!(get("meeting_timezone"), "UTC");
        assert_eq!(get("full_day"), "y");
        assert_eq!(get("information"), "Back on the 5th");
    }

    /// A session against `server`, as if logged in.
    fn session(server: &MockServer) -> Session {
        let host = host_of(&server.uri()).to_string();
        let jar = format!("{host}\tFALSE\t/\tFALSE\t0\tsession\ts1\n");
        login(&server.uri(), &jar, &format!("{}/", server.uri())).unwrap()
    }

    const FORM: &str = r#"<input name="csrf_token" type="hidden" value="t1">"#;

    #[tokio::test]
    async fn a_submission_carries_the_session_the_form_set() {
        let server = MockServer::start().await;
        // Opening the form starts a new session holding the CSRF secret.
        Mock::given(method("GET"))
            .and(path("/vacation/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("set-cookie", "session=with-csrf; Secure; HttpOnly; Path=/")
                    .set_body_string(FORM),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/vacation/add/"))
            .and(header("cookie", "session=with-csrf"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "/vacation/2026/12/21/"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let entry = NewEntry::full_days("vacation", "PTO", day("2026-12-21"), day("2027-01-04"));
        session(&server).add(&entry).await.unwrap();
        // A form shown again without any message: what that usually means.
        assert!(
            Error::Refused(vec![])
                .to_string()
                .contains("rejected CSRF token or an expired session")
        );
        assert_eq!(
            page_messages(r#"<tr class="errors"><td>Not a valid date value.</td></tr>"#),
            ["Not a valid date value."]
        );
    }

    #[tokio::test]
    async fn an_entry_is_added_with_the_forms_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/vacation/add/"))
            .and(header("cookie", "session=s1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/vacation/add/"))
            .and(body_string_contains("csrf_token=t1"))
            .and(body_string_contains("meeting_date_end=2027-01-04"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "/vacation/2026/12/21/"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let entry = NewEntry::full_days("vacation", "PTO", day("2026-12-21"), day("2027-01-04"));
        let s = session(&server);
        s.can_add("vacation").await.unwrap();
        s.add(&entry).await.unwrap();
    }

    #[test]
    fn only_a_redirect_to_the_filed_day_means_filed() {
        let base = "https://calendar.fedoraproject.org";
        assert!(is_filed_redirect("/vacation/2026/12/21/", base, "vacation"));
        assert!(is_filed_redirect(
            "https://calendar.fedoraproject.org/vacation/2026/12/21/",
            base,
            "vacation"
        ));
        assert!(!is_filed_redirect("/vacation/", base, "vacation"));
        assert!(!is_filed_redirect("/", base, "vacation"));
        assert!(!is_filed_redirect("/login?next=x", base, "vacation"));
        assert!(!is_filed_redirect("/other/2026/12/21/", base, "vacation"));
        assert!(!is_filed_redirect(
            "https://evil.example/vacation/2026/1/1/",
            base,
            "vacation"
        ));
    }

    #[tokio::test]
    async fn a_redirect_elsewhere_is_a_refusal() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/vacation/add/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .mount(&server)
            .await;
        // An expired session: back to the front page, with a message.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<li class="errors">You must be logged in</li>"#),
            )
            .mount(&server)
            .await;
        let entry = NewEntry::full_days("vacation", "PTO", day("2026-12-21"), day("2027-01-04"));
        let err = session(&server).add(&entry).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Fedora Calendar refused the entry: You must be logged in"
        );
    }

    /// An edit page shaped like fedocal's.
    const EDIT: &str = "<html><form action=\"/search/\"><input name=\"keyword\"></form>\
        <form action=\"\" method=\"post\">\
        <select id=\"calendar_name\" name=\"calendar_name\"><option value=\"other\">other</option>\
        <option selected value=\"vacation\">vacation</option></select>\
        <input id=\"meeting_name\" name=\"meeting_name\" type=\"text\" value=\"me &amp; co\">\
        <input name=\"meeting_date\" type=\"date\" value=\"2026-11-24\">\
        <input name=\"meeting_date_end\" type=\"date\" value=\"2027-01-04\">\
        <input checked id=\"full_day\" name=\"full_day\" type=\"checkbox\" value=\"y\">\
        <input name=\"unchecked\" type=\"checkbox\" value=\"y\">\
        <select name=\"frequency\"><option value=\"\"></option><option value=\"7\">7 days</option></select>\
        <input name=\"comanager\" type=\"text\" value=\"me\">\
        <textarea id=\"information\" name=\"information\">\r\n</textarea>\
        <input type=\"button\" value=\"Preview\">\
        <input id=\"csrf_token\" name=\"csrf_token\" type=\"hidden\" value=\"t2\">\
        <input name=\"action\" type=\"submit\" value=\"Save\">\
        <input name=\"action\" type=\"button\" value=\"Cancel\"></form></html>";

    #[test]
    fn an_edit_form_reads_as_a_browser_sends_it() {
        let fields = form_values(EDIT).unwrap();
        let shown: Vec<String> = fields.iter().map(|(n, v)| format!("{n}={v}")).collect();
        assert_eq!(
            shown,
            [
                "calendar_name=vacation",
                "meeting_name=me & co",
                "meeting_date=2026-11-24",
                "meeting_date_end=2027-01-04",
                "full_day=y",
                "frequency=",
                "comanager=me",
                "information=",
                "csrf_token=t2",
                "action=Save"
            ]
        );
        assert!(form_values("<form></form>").is_none());
        // New days are sent as the day after the last; the rest is kept.
        let mut fields = fields;
        let changes = Changes::default()
            .days(day("2026-11-24"), day("2027-01-01"))
            .information("Back on the 4th");
        assert!(!changes.is_empty() && Changes::default().is_empty());
        changes.apply(&mut fields);
        let get = |k: &str| fields.iter().find(|(n, _)| n == k).unwrap().1.clone();
        assert_eq!(get("meeting_date_end"), "2027-01-02");
        assert_eq!(get("information"), "Back on the 4th");
        assert_eq!(get("meeting_name"), "me & co");
        Changes::default().name("PTO").apply(&mut fields);
        let get = |k: &str| fields.iter().find(|(n, _)| n == k).unwrap().1.clone();
        assert_eq!(get("meeting_name"), "PTO");
        assert_eq!(
            parse_attrs("href=/meeting/"),
            [("href".to_string(), Some("/meeting/".to_string()))]
        );
        let texted = "<form method=\"post\"><select name=\"tz\"><option>UTC</option>\
            <option selected>Europe/Dublin</option></select>\
            <input name=\"csrf_token\" type=\"hidden\" value=\"t\"></form>";
        assert_eq!(
            form_values(texted).unwrap()[0],
            ("tz".to_string(), "Europe/Dublin".to_string())
        );
        assert_eq!(
            parse_attrs("a=1 b='two' c d = \"x y\" /"),
            [
                ("a".to_string(), Some("1".to_string())),
                ("b".to_string(), Some("two".to_string())),
                ("c".to_string(), None),
                ("d".to_string(), Some("x y".to_string()))
            ]
        );
    }

    #[tokio::test]
    async fn an_entry_is_edited_keeping_what_is_not_changed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/11/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(EDIT))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/11/"))
            .and(body_string_contains("information=Back+on+the+4th"))
            .and(body_string_contains("comanager=me"))
            .and(body_string_contains("meeting_date_end=2027-01-04"))
            .and(body_string_contains("csrf_token=t2"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/11/"))
            .expect(1)
            .mount(&server)
            .await;
        let s = session(&server);
        s.can_edit(11).await.unwrap();
        s.edit(11, &Changes::default().information("Back on the 4th"))
            .await
            .unwrap();
        // Led anywhere but the entry: a refusal, with the page's reasons.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/11/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(EDIT))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<li class="errors">You are not allowed to edit this meeting</li>"#,
            ))
            .mount(&server)
            .await;
        let err = session(&server)
            .edit(11, &Changes::default().name("x"))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .ends_with("You are not allowed to edit this meeting"),
            "{err}"
        );
        // No form at all: the reasons, or an expired session.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
            .mount(&server)
            .await;
        let err = session(&server)
            .edit(11, &Changes::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no edit form"), "{err}");
        assert_eq!(
            path_of("http://h:1/meeting/11/?x", "http://h:1"),
            Some("/meeting/11/".to_string())
        );
        assert_eq!(path_of("http://other/meeting/11/", "http://h:1"), None);
    }

    #[tokio::test]
    async fn a_refusal_carries_fedocals_reasons() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<li class="warnings">The start date of your meeting is later than the stop date.</li>"#,
            ))
            .mount(&server)
            .await;
        let entry = NewEntry::full_days("vacation", "PTO", day("2027-01-04"), day("2026-12-21"));
        let err = session(&server).add(&entry).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Fedora Calendar refused the entry: The start date of your meeting is later than \
             the stop date."
        );
    }

    #[tokio::test]
    async fn a_calendar_the_user_may_not_add_to_says_why() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/vacation/add/"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/vacation/"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/vacation/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<li class="errors">You are not one of the editors of this calendar</li>"#,
            ))
            .mount(&server)
            .await;
        let entry = NewEntry::full_days("vacation", "PTO", day("2026-12-21"), day("2027-01-04"));
        let err = session(&server).add(&entry).await.unwrap_err();
        assert!(
            matches!(err, Error::Refused(ref r) if r[0].starts_with("You are not")),
            "{err}"
        );
        // A page without a token or a reason: an expired session.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
            .mount(&server)
            .await;
        let err = session(&server).add(&entry).await.unwrap_err();
        assert!(err.to_string().contains("no CSRF token"), "{err}");
        // A server error on submitting.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let err = session(&server).add(&entry).await.unwrap_err();
        assert!(
            err.to_string().ends_with("HTTP 500 Internal Server Error"),
            "{err}"
        );
    }
    /// Matches a request whose body ends with the text given.
    struct EndsWith(&'static str);

    impl wiremock::Match for EndsWith {
        fn matches(&self, request: &wiremock::Request) -> bool {
            request.body.ends_with(self.0.as_bytes())
        }
    }

    /// A recurring entry's edit page: "Save" and "Save all".
    const SERIES: &str = "<form action=\"\" method=\"post\">\
        <input name=\"meeting_name\" type=\"text\" value=\"Hangout\">\
        <input name=\"meeting_date\" type=\"date\" value=\"2026-12-02\">\
        <input name=\"meeting_location\" type=\"text\" value=\"\">\
        <select name=\"frequency\"><option value=\"\"></option><option selected value=\"28\">4 weeks</option></select>\
        <input name=\"end_repeats\" type=\"date\" value=\"2037-12-31\">\
        <input name=\"csrf_token\" type=\"hidden\" value=\"t3\">\
        <input name=\"action\" type=\"submit\" value=\"Save\">\
        <input name=\"action\" type=\"submit\" value=\"Save all\">\
        <input name=\"action\" type=\"button\" value=\"Cancel\"></form>";

    #[tokio::test]
    async fn a_series_is_edited_whole_or_one_occurrence() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/5/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SERIES))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/5/"))
            .and(body_string_contains("action=Save+all"))
            .and(body_string_contains(
                "meeting_location=https%3A%2F%2Fmeet.example%2Fx",
            ))
            .and(body_string_contains("end_repeats=2027-06-30"))
            .and(body_string_contains("frequency=28"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/5/"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/5/"))
            .and(EndsWith("action=Save"))
            .and(body_string_contains("meeting_name=Special"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/5/"))
            .expect(1)
            .mount(&server)
            .await;
        let s = session(&server);
        let form = s.entry(5).await.unwrap();
        assert!(form.recurring());
        assert_eq!(form.submits, ["Save", "Save all"]);
        assert_eq!(form.get("end_repeats"), Some("2037-12-31"));
        assert_eq!(form.get("nothing"), None);
        let whole = Changes::default()
            .location("https://meet.example/x")
            .until(day("2027-06-30"))
            .whole_series(true);
        assert!(!whole.is_empty());
        s.edit(5, &whole).await.unwrap();
        s.edit(5, &Changes::default().name("Special"))
            .await
            .unwrap();
        assert!(!EntryForm::default().recurring());
        assert!(submit_values("<form></form>").is_empty());
    }

    #[tokio::test]
    async fn an_occurrence_is_cancelled_through_the_delete_form() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/delete/9/"))
            .and(wiremock::matchers::query_param("from_date", "2026-11-04"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/delete/9/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/delete/9/"))
            .and(body_string_contains("csrf_token=t1"))
            .and(body_string_contains("confirm_delete=y"))
            .and(body_string_contains("from_date=2026-11-04"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/cal/2026/11/4/"))
            .expect(1)
            .mount(&server)
            .await;
        let s = session(&server);
        s.can_cancel(9).await.unwrap();
        s.cancel(9, day("2026-11-04")).await.unwrap();
        // Refused after the form: fedocal redirects with the reason.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/delete/9/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/9/"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/9/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"<li class="errors">you are not allowed to delete it.</li>"#,
                ),
            )
            .mount(&server)
            .await;
        let err = session(&server)
            .cancel(9, day("2026-11-04"))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .ends_with("you are not allowed to delete it."),
            "{err}"
        );
        // Not allowed: the form is not there, and the reason is said.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<li class="errors">You are not one of the manager of this meeting</li>"#,
            ))
            .mount(&server)
            .await;
        let err = session(&server)
            .cancel(9, day("2026-11-04"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not one of the manager"), "{err}");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html></html>"))
            .mount(&server)
            .await;
        let err = session(&server).can_cancel(9).await.unwrap_err();
        assert!(err.to_string().contains("no delete form"), "{err}");
        // A refusal on submitting, or a server error.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FORM))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<li class="errors">Not a valid date value</li>"#),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let s = session(&server);
        let err = s.cancel(9, day("2026-11-04")).await.unwrap_err();
        assert!(err.to_string().ends_with("Not a valid date value"), "{err}");
        let err = s.cancel(9, day("2026-11-04")).await.unwrap_err();
        assert!(
            err.to_string().ends_with("HTTP 500 Internal Server Error"),
            "{err}"
        );
    }
}
