// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Koji build system API client.

use std::path::Path;
use std::process::Command;

use sandogasa_kojihub::{Client, Error, Value, retry};

/// Default number of retry attempts for transient failures.
const DEFAULT_RETRIES: u32 = 3;

/// Task state constants from Koji (re-exported from the shared
/// hub crate; local definitions (rather than re-exports) keep
/// `koji::TASK_CLOSED` callers working AND visible to rustdoc /
/// cargo-semver-checks, which don't inline foreign re-exports.
pub const TASK_CLOSED: i64 = sandogasa_kojihub::hub::TASK_CLOSED;
pub const TASK_FAILED: i64 = sandogasa_kojihub::hub::TASK_FAILED;

/// Koji API client.
pub struct KojiClient {
    client: Client,
    instance: String,
}

/// Information about a Koji build.
#[derive(Debug)]
pub struct BuildInfo {
    pub id: i64,
    pub task_id: i64,
    pub name: String,
    pub version: String,
    pub release: String,
    pub nvr: String,
    pub state: i64,
}

/// Information about a Koji task.
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: i64,
    pub method: String,
    pub arch: String,
    pub state: i64,
}

impl TaskInfo {
    pub fn state_name(&self) -> &'static str {
        match self.state {
            0 => "FREE",
            1 => "OPEN",
            2 => "CLOSED",
            3 => "CANCELED",
            4 => "ASSIGNED",
            5 => "FAILED",
            _ => "UNKNOWN",
        }
    }
}

impl KojiClient {
    pub fn new(instance: &str) -> Self {
        Self::with_hub_url(instance, &format!("https://{instance}/kojihub"))
    }

    /// A client for `instance` whose hub is at `hub_url`.
    fn with_hub_url(instance: &str, hub_url: &str) -> Self {
        Self {
            client: Client::new(hub_url),
            instance: instance.to_string(),
        }
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Fetch build information by build ID.
    pub fn get_build(&self, build_id: i64) -> Result<BuildInfo, Error> {
        let result = retry(DEFAULT_RETRIES, || {
            self.client.call("getBuild", &[Value::Int(build_id)])
        })?;
        Ok(BuildInfo {
            id: result
                .get("id")
                .and_then(|v| v.as_int())
                .unwrap_or(build_id),
            task_id: result
                .get("task_id")
                .and_then(|v| v.as_int())
                .ok_or_else(|| Error::Parse("missing task_id in build info".into()))?,
            name: result
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            version: result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            release: result
                .get("release")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            nvr: result
                .get("nvr")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            state: result.get("state").and_then(|v| v.as_int()).unwrap_or(-1),
        })
    }

    /// Fetch task information by task ID.
    pub fn get_task_info(&self, task_id: i64) -> Result<TaskInfo, Error> {
        let result = retry(DEFAULT_RETRIES, || {
            self.client.call("getTaskInfo", &[Value::Int(task_id)])
        })?;
        Ok(TaskInfo {
            id: result.get("id").and_then(|v| v.as_int()).unwrap_or(task_id),
            method: result
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            arch: result
                .get("arch")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            state: result.get("state").and_then(|v| v.as_int()).unwrap_or(-1),
        })
    }

    /// Fetch child tasks of a parent task.
    pub fn get_task_children(&self, task_id: i64) -> Result<Vec<TaskInfo>, Error> {
        let result = retry(DEFAULT_RETRIES, || {
            self.client.call("getTaskChildren", &[Value::Int(task_id)])
        })?;
        let items = result
            .as_array()
            .ok_or_else(|| Error::Parse("expected array for task children".into()))?;
        items
            .iter()
            .map(|v| {
                Ok(TaskInfo {
                    id: v
                        .get("id")
                        .and_then(|v| v.as_int())
                        .ok_or_else(|| Error::Parse("missing id in child task".into()))?,
                    method: v
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    arch: v
                        .get("arch")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    state: v.get("state").and_then(|v| v.as_int()).unwrap_or(-1),
                })
            })
            .collect()
    }

    /// Resolve a task ID to the buildArch task for the given architecture.
    ///
    /// If the task is already a buildArch task, returns it directly.
    /// If it is a parent build task, finds the matching child.
    pub fn resolve_build_arch_task(&self, task_id: i64, arch: &str) -> Result<TaskInfo, Error> {
        let info = self.get_task_info(task_id)?;
        if info.method == "buildArch" {
            return Ok(info);
        }

        let children = self.get_task_children(task_id)?;
        let arch_tasks: Vec<_> = children
            .iter()
            .filter(|t| t.method == "buildArch" && t.arch == arch)
            .collect();

        if arch_tasks.is_empty() {
            let available: Vec<_> = children
                .iter()
                .filter(|t| t.method == "buildArch")
                .map(|t| t.arch.as_str())
                .collect();
            return Err(Error::Parse(format!(
                "no buildArch task for arch '{arch}' under task {task_id}; \
                 available: {}",
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            )));
        }

        Ok(arch_tasks[0].clone())
    }

    /// Download a log file from a task using `koji download-logs`.
    ///
    /// Downloads into `dest_dir` and returns the content of `filename`.
    pub fn download_log(
        &self,
        task_id: i64,
        filename: &str,
        dest_dir: &Path,
    ) -> Result<String, Error> {
        let profile = self.koji_profile();
        retry(DEFAULT_RETRIES, || {
            let mut cmd = Command::new("koji");
            if let Some(p) = profile {
                cmd.arg("-p").arg(p);
            }
            let output = cmd
                .arg("download-logs")
                .arg("--dir")
                .arg(dest_dir)
                .arg(task_id.to_string())
                .output()
                .map_err(|e| Error::Parse(format!("failed to run koji download-logs: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(Error::Parse(format!(
                    "koji download-logs {task_id} failed: {stderr}"
                )));
            }
            Ok(())
        })?;

        read_downloaded_log(dest_dir, filename, task_id)
    }

    /// Download a log file from a build's persistent storage via HTTP.
    ///
    /// Build logs are stored at:
    /// `https://{topurl}/packages/{name}/{version}/{release}/data/logs/{arch}/{filename}`
    ///
    /// These persist even after task logs are garbage collected.
    pub fn download_build_log(
        &self,
        build: &BuildInfo,
        arch: &str,
        filename: &str,
    ) -> Result<String, Error> {
        let topurl = self.packages_url().ok_or_else(|| {
            Error::Parse(format!(
                "no known packages URL for instance {}",
                self.instance
            ))
        })?;
        fetch_text(&build_log_url(topurl, build, arch, filename))
    }

    /// Return the packages base URL for this instance, if known.
    fn packages_url(&self) -> Option<&'static str> {
        match self.instance.as_str() {
            "koji.fedoraproject.org" => Some("https://kojipkgs.fedoraproject.org"),
            "cbs.centos.org" => Some("https://cbs.centos.org/kojifiles"),
            "kojihub.stream.centos.org" => Some("https://kojihub.stream.centos.org/kojifiles"),
            _ => None,
        }
    }

    /// Return the koji CLI profile for this instance, if any.
    fn koji_profile(&self) -> Option<&'static str> {
        match self.instance.as_str() {
            "koji.fedoraproject.org" => None,
            "cbs.centos.org" => Some("cbs"),
            "kojihub.stream.centos.org" => Some("stream"),
            _ => None,
        }
    }
}

/// Where a build's `filename` for `arch` is kept under `topurl`.
fn build_log_url(topurl: &str, build: &BuildInfo, arch: &str, filename: &str) -> String {
    format!(
        "{topurl}/packages/{name}/{version}/{release}/data/logs/{arch}/{filename}",
        name = build.name,
        version = build.version,
        release = build.release,
    )
}

/// The body of `url`, retrying transient failures.
fn fetch_text(url: &str) -> Result<String, Error> {
    retry(DEFAULT_RETRIES, || {
        let response = reqwest::blocking::get(url)
            .map_err(|e| Error::Parse(format!("failed to fetch {url}: {e}")))?;
        if !response.status().is_success() {
            return Err(Error::Parse(format!("{url}: HTTP {}", response.status())));
        }
        response
            .text()
            .map_err(|e| Error::Parse(format!("failed to read {url}: {e}")))
    })
}

/// Read `filename` from what `koji download-logs` left in `dest_dir`:
/// at the top, or anywhere below, as its layout varies. When it is not
/// there, the error lists what was downloaded.
fn read_downloaded_log(dest_dir: &Path, filename: &str, task_id: i64) -> Result<String, Error> {
    let path = dest_dir.join(filename);
    if path.exists() {
        return std::fs::read_to_string(&path).map_err(|e| {
            Error::Parse(format!("failed to read {filename} for task {task_id}: {e}"))
        });
    }

    if let Some(found) = find_file_recursive(dest_dir, filename) {
        return std::fs::read_to_string(&found)
            .map_err(|e| Error::Parse(format!("failed to read {}: {e}", found.display())));
    }

    // List what was actually downloaded for diagnostics.
    let mut files = Vec::new();
    collect_files(dest_dir, &mut files);
    Err(Error::Parse(format!(
        "{filename} not found for task {task_id}\n\
         koji download-logs produced: {}",
        if files.is_empty() {
            "(no files)".to_string()
        } else {
            files
                .iter()
                .map(|p| p.strip_prefix(dest_dir).unwrap_or(p).display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    )))
}

/// Recursively find a file by name under a directory.
pub fn find_file_in(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    find_file_recursive(dir, name)
}

fn find_file_recursive(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file_recursive(&path, name)
        {
            return Some(found);
        }
    }
    None
}

/// Collect all file paths under a directory.
fn collect_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                out.push(path);
            } else if path.is_dir() {
                collect_files(&path, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Start a mock server, each `(xml-rpc method, response)` answering
    /// calls to that method; the runtime keeps it alive.
    fn hub(answers: &[(&str, String)]) -> (tokio::runtime::Runtime, MockServer) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        for (name, body) in answers {
            rt.block_on(
                Mock::given(method("POST"))
                    .and(body_string_contains(format!(
                        "<methodName>{name}</methodName>"
                    )))
                    .respond_with(ResponseTemplate::new(200).set_body_string(body.clone()))
                    .mount(&server),
            );
        }
        (rt, server)
    }

    fn response(value: &str) -> String {
        format!(
            "<?xml version='1.0'?><methodResponse><params><param>{value}</param>\
             </params></methodResponse>"
        )
    }

    fn task(id: i64, method: &str, arch: &str, state: i64) -> String {
        format!(
            "<value><struct>\
             <member><name>id</name><value><int>{id}</int></value></member>\
             <member><name>method</name><value><string>{method}</string></value></member>\
             <member><name>arch</name><value><string>{arch}</string></value></member>\
             <member><name>state</name><value><int>{state}</int></value></member>\
             </struct></value>"
        )
    }

    fn children(tasks: &[String]) -> String {
        response(&format!(
            "<value><array><data>{}</data></array></value>",
            tasks.concat()
        ))
    }

    fn client(server: &MockServer) -> KojiClient {
        KojiClient::with_hub_url("koji.example.org", &server.uri())
    }

    #[test]
    fn a_build_is_read_from_getbuild() {
        let build = response(
            "<value><struct>\
             <member><name>id</name><value><int>42</int></value></member>\
             <member><name>task_id</name><value><int>7</int></value></member>\
             <member><name>name</name><value><string>foo</string></value></member>\
             <member><name>version</name><value><string>1.0</string></value></member>\
             <member><name>release</name><value><string>1.fc45</string></value></member>\
             <member><name>nvr</name><value><string>foo-1.0-1.fc45</string></value></member>\
             <member><name>state</name><value><int>1</int></value></member>\
             </struct></value>",
        );
        let (_rt, server) = hub(&[("getBuild", build)]);
        let b = client(&server).get_build(42).unwrap();
        assert_eq!((b.id, b.task_id, b.state), (42, 7, 1));
        assert_eq!(b.nvr, "foo-1.0-1.fc45");
        assert_eq!((b.name.as_str(), b.version.as_str()), ("foo", "1.0"));
        assert_eq!(b.release, "1.fc45");
    }

    #[test]
    fn a_build_without_a_task_is_an_error() {
        // An imported build has no task to diff logs from.
        let build = response(
            "<value><struct>\
             <member><name>nvr</name><value><string>foo-1-1</string></value></member>\
             </struct></value>",
        );
        let (_rt, server) = hub(&[("getBuild", build)]);
        let err = client(&server).get_build(42).unwrap_err();
        assert!(err.to_string().contains("missing task_id"), "{err}");
    }

    #[test]
    fn a_buildarch_task_resolves_to_itself() {
        let (_rt, server) = hub(&[(
            "getTaskInfo",
            response(&task(9, "buildArch", "x86_64", TASK_CLOSED)),
        )]);
        let t = client(&server)
            .resolve_build_arch_task(9, "aarch64")
            .unwrap();
        assert_eq!((t.id, t.arch.as_str()), (9, "x86_64"));
        assert_eq!(t.state_name(), "CLOSED");
    }

    #[test]
    fn a_build_task_resolves_to_its_child_for_the_arch() {
        let (_rt, server) = hub(&[
            ("getTaskInfo", response(&task(1, "build", "noarch", 2))),
            (
                "getTaskChildren",
                children(&[
                    task(2, "buildSRPMFromSCM", "noarch", 2),
                    task(3, "buildArch", "x86_64", 2),
                    task(4, "buildArch", "aarch64", TASK_FAILED),
                ]),
            ),
        ]);
        let t = client(&server)
            .resolve_build_arch_task(1, "aarch64")
            .unwrap();
        assert_eq!(t.id, 4);
        assert_eq!(t.state_name(), "FAILED");
    }

    #[test]
    fn a_missing_arch_names_the_arches_there_are() {
        let (_rt, server) = hub(&[
            ("getTaskInfo", response(&task(1, "build", "noarch", 2))),
            (
                "getTaskChildren",
                children(&[
                    task(3, "buildArch", "x86_64", 2),
                    task(4, "buildArch", "aarch64", 2),
                ]),
            ),
        ]);
        let err = client(&server)
            .resolve_build_arch_task(1, "s390x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no buildArch task for arch 's390x'"), "{err}");
        assert!(err.contains("available: x86_64, aarch64"), "{err}");
    }

    #[test]
    fn a_task_without_buildarch_children_says_none() {
        let (_rt, server) = hub(&[
            ("getTaskInfo", response(&task(1, "build", "noarch", 2))),
            ("getTaskChildren", children(&[])),
        ]);
        let err = client(&server)
            .resolve_build_arch_task(1, "x86_64")
            .unwrap_err()
            .to_string();
        assert!(err.contains("available: (none)"), "{err}");
    }

    #[test]
    fn a_child_task_without_an_id_is_an_error() {
        let child = response(
            "<value><array><data><value><struct>\
             <member><name>method</name><value><string>buildArch</string></value></member>\
             </struct></value></data></array></value>",
        );
        let (_rt, server) = hub(&[("getTaskChildren", child)]);
        let err = client(&server).get_task_children(1).unwrap_err();
        assert!(err.to_string().contains("missing id"), "{err}");
        let (_rt, server) = hub(&[("getTaskChildren", response("<value><int>1</int></value>"))]);
        let err = client(&server).get_task_children(1).unwrap_err();
        assert!(err.to_string().contains("expected array"), "{err}");
    }

    #[test]
    fn state_names_cover_every_koji_task_state() {
        let names: Vec<&str> = (0..=6)
            .map(|state| {
                TaskInfo {
                    id: 0,
                    method: String::new(),
                    arch: String::new(),
                    state,
                }
                .state_name()
            })
            .collect();
        assert_eq!(
            names,
            [
                "FREE", "OPEN", "CLOSED", "CANCELED", "ASSIGNED", "FAILED", "UNKNOWN"
            ]
        );
    }

    #[test]
    fn instances_map_to_their_packages_url_and_cli_profile() {
        let fedora = KojiClient::new("koji.fedoraproject.org");
        assert_eq!(fedora.instance(), "koji.fedoraproject.org");
        assert_eq!(
            fedora.packages_url(),
            Some("https://kojipkgs.fedoraproject.org")
        );
        assert_eq!(fedora.koji_profile(), None);
        let cbs = KojiClient::new("cbs.centos.org");
        assert_eq!(cbs.packages_url(), Some("https://cbs.centos.org/kojifiles"));
        assert_eq!(cbs.koji_profile(), Some("cbs"));
        let stream = KojiClient::new("kojihub.stream.centos.org");
        assert_eq!(stream.koji_profile(), Some("stream"));
        let other = KojiClient::new("koji.example.org");
        assert_eq!((other.packages_url(), other.koji_profile()), (None, None));
    }

    fn build() -> BuildInfo {
        BuildInfo {
            id: 1,
            task_id: 2,
            name: "foo".into(),
            version: "1.0".into(),
            release: "1.fc45".into(),
            nvr: "foo-1.0-1.fc45".into(),
            state: 1,
        }
    }

    #[test]
    fn a_build_log_is_fetched_from_the_packages_tree() {
        assert_eq!(
            build_log_url("https://pkgs.example", &build(), "x86_64", "build.log"),
            "https://pkgs.example/packages/foo/1.0/1.fc45/data/logs/x86_64/build.log"
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        rt.block_on(
            Mock::given(method("GET"))
                .and(path("/packages/foo/1.0/1.fc45/data/logs/x86_64/build.log"))
                .respond_with(ResponseTemplate::new(200).set_body_string("the log"))
                .mount(&server),
        );
        let url = build_log_url(&server.uri(), &build(), "x86_64", "build.log");
        assert_eq!(fetch_text(&url).unwrap(), "the log");
        let missing = build_log_url(&server.uri(), &build(), "s390x", "build.log");
        let err = fetch_text(&missing).unwrap_err().to_string();
        assert!(err.contains("HTTP 404"), "{err}");
    }

    #[test]
    fn a_build_log_needs_a_known_packages_url() {
        let err = KojiClient::new("koji.example.org")
            .download_build_log(&build(), "x86_64", "build.log")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no known packages URL"), "{err}");
    }

    #[test]
    fn a_downloaded_log_is_found_wherever_koji_put_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("root.log"), "top").unwrap();
        assert_eq!(
            read_downloaded_log(dir.path(), "root.log", 1).unwrap(),
            "top"
        );
        let nested = dir.path().join("kojilogs/x86_64-9");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("build.log"), "nested").unwrap();
        assert_eq!(
            read_downloaded_log(dir.path(), "build.log", 9).unwrap(),
            "nested"
        );
        assert_eq!(
            find_file_in(dir.path(), "build.log"),
            Some(nested.join("build.log"))
        );
        let err = read_downloaded_log(dir.path(), "state.log", 9)
            .unwrap_err()
            .to_string();
        assert!(err.contains("state.log not found for task 9"), "{err}");
        assert!(err.contains("root.log"), "{err}");
        assert!(err.contains("build.log"), "{err}");
        let empty = tempfile::tempdir().unwrap();
        let err = read_downloaded_log(empty.path(), "build.log", 9)
            .unwrap_err()
            .to_string();
        assert!(err.contains("(no files)"), "{err}");
    }
}
