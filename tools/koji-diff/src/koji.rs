// SPDX-License-Identifier: MPL-2.0

//! Koji build system API client.

use std::path::Path;
use std::process::Command;

use crate::xmlrpc::{Client, Error, Value};

/// Task state constants from Koji.
pub const TASK_CLOSED: i64 = 2;
pub const TASK_FAILED: i64 = 5;

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
        let hub_url = format!("https://{instance}/kojihub");
        Self {
            client: Client::new(&hub_url),
            instance: instance.to_string(),
        }
    }

    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Fetch build information by build ID.
    pub fn get_build(&self, build_id: i64) -> Result<BuildInfo, Error> {
        let result = self.client.call("getBuild", &[Value::Int(build_id)])?;
        Ok(BuildInfo {
            id: result
                .get("id")
                .and_then(|v| v.as_int())
                .unwrap_or(build_id),
            task_id: result
                .get("task_id")
                .and_then(|v| v.as_int())
                .ok_or_else(|| Error::Parse("missing task_id in build info".into()))?,
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
        let result = self.client.call("getTaskInfo", &[Value::Int(task_id)])?;
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
        let result = self
            .client
            .call("getTaskChildren", &[Value::Int(task_id)])?;
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
        let mut cmd = Command::new("koji");

        if let Some(profile) = self.koji_profile() {
            cmd.arg("-p").arg(profile);
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

        let path = dest_dir.join(filename);
        if path.exists() {
            return std::fs::read_to_string(&path).map_err(|e| {
                Error::Parse(format!("failed to read {filename} for task {task_id}: {e}"))
            });
        }

        // koji download-logs may use a different layout — search for the
        // file anywhere under dest_dir.
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
        if path.is_dir() {
            if let Some(found) = find_file_recursive(&path, name) {
                return Some(found);
            }
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
