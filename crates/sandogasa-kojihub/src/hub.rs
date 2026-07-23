// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed layer over the Koji hub methods the sandogasa tools use.
//!
//! Field notes (validated live against Fedora's hub): `listTasks`
//! with `decode: true` returns per-task structs whose timestamps
//! are UTC unix doubles (`create_ts`/`start_ts`/`completion_ts`);
//! queue wait is `start_ts - create_ts` and build time is
//! `completion_ts - start_ts`. The parallel `*_time` string fields
//! are never parsed here. Task IDs can exceed i32 (the XML uses
//! `<i8>`, which the wire layer already decodes).

use std::collections::{BTreeMap, HashMap};

use crate::xmlrpc::{Client, Error, Value};

/// Task state: free (not yet taken by a builder).
pub const TASK_FREE: i64 = 0;
/// Task state: open (running on a builder).
pub const TASK_OPEN: i64 = 1;
/// Task state: closed (completed successfully).
pub const TASK_CLOSED: i64 = 2;
/// Task state: canceled.
pub const TASK_CANCELED: i64 = 3;
/// Task state: assigned to a builder, not yet started.
pub const TASK_ASSIGNED: i64 = 4;
/// Task state: failed.
pub const TASK_FAILED: i64 = 5;

/// Filters for `listTasks` (the subset the tools need).
#[derive(Debug, Clone, Default)]
pub struct ListTasksOpts {
    /// Task method (e.g. `build`, `buildArch`).
    pub method: Option<String>,
    /// Only tasks completed at/after this UTC unix timestamp.
    pub complete_after: Option<f64>,
    /// Only tasks completed at/before this UTC unix timestamp.
    pub complete_before: Option<f64>,
    /// Only children of these parent task IDs. This filter hits
    /// koji's `task(parent)` index, so it stays fast (measured
    /// ~0.5s) even when completion-window filtering is melting
    /// down under hub load — prefer it wherever the parents are
    /// already known.
    pub parent: Option<Vec<i64>>,
    /// Decode the task request into structured values.
    pub decode: bool,
}

/// Pagination / ordering for hub list methods.
#[derive(Debug, Clone, Default)]
pub struct QueryOpts {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Result ordering (e.g. `id`).
    pub order: Option<String>,
}

/// One task as returned by `listTasks` / `getTaskInfo`. Every
/// field the hub may omit (or send as nil) is an `Option` so
/// older hubs and sparse records decode without errors.
#[derive(Debug, Clone)]
pub struct HubTask {
    pub id: i64,
    pub parent: Option<i64>,
    pub method: String,
    pub arch: Option<String>,
    pub state: i64,
    pub create_ts: Option<f64>,
    pub start_ts: Option<f64>,
    pub completion_ts: Option<f64>,
    pub host_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub owner: Option<i64>,
    pub owner_name: Option<String>,
    pub priority: Option<i64>,
    pub weight: Option<f64>,
    /// Decoded request (an array); present when `decode` was
    /// requested. The caller interprets its positional layout.
    pub request: Option<Value>,
}

/// Typed client for the Koji hub.
pub struct HubClient {
    client: Client,
    hub_url: String,
}

impl HubClient {
    pub fn new(hub_url: &str) -> Self {
        let hub_url = hub_url.trim_end_matches('/').to_string();
        Self {
            client: Client::new(&hub_url),
            hub_url,
        }
    }

    pub fn url(&self) -> &str {
        &self.hub_url
    }

    /// One page of `listTasks(opts, queryOpts)`.
    pub fn list_tasks(
        &self,
        opts: &ListTasksOpts,
        query: &QueryOpts,
    ) -> Result<Vec<HubTask>, Error> {
        let result = self
            .client
            .call("listTasks", &[opts.to_value(), query.to_value()])?;
        let items = result
            .as_array()
            .ok_or_else(|| Error::Parse("listTasks did not return an array".to_string()))?;
        items.iter().map(task_from_value).collect()
    }

    /// Sweep a completion-time window of `listTasks` results by
    /// **bisection**: fetch `[after, before]` unordered with a
    /// `page_size` limit; a full page means the slice may be
    /// truncated, so split it in half and recurse (left first).
    ///
    /// Why not OFFSET pagination: measured against Fedora's hub,
    /// `order: id` turns a 7-second page into a 65+-second one
    /// (and offsets degrade linearly), while unordered
    /// completion-range slices stay fast — but unordered results
    /// make offsets unstable, hence bisection. Slice boundaries
    /// can duplicate a task (inclusive bounds); results are
    /// deduped by id here, and datasets dedupe again on merge.
    ///
    /// Each slice request is retried `retries` times via
    /// [`crate::retry`]. `on_page` runs per fetched slice, for
    /// progress output and caller-paced sleeps. A slice narrower
    /// than one second that still fills a page is accepted with a
    /// warning (theoretical truncation; would need `page_size`
    /// tasks completing within the same second).
    ///
    /// `seed_span` caps the *initial* slice width (seconds): the
    /// window is pre-cut into seed-sized slices before bisection.
    /// Completion filtering cost scales with the matching set, so
    /// starting from the whole window front-loads the heaviest
    /// requests only to discard and re-fetch them as halves —
    /// seeding keeps every request modest. Pass `f64::INFINITY`
    /// to start from the whole window.
    #[allow(clippy::too_many_arguments)]
    pub fn sweep_completion_window(
        &self,
        opts: &ListTasksOpts,
        after: f64,
        before: f64,
        page_size: i64,
        retries: u32,
        seed_span: f64,
        on_page: &mut impl FnMut(&[HubTask]),
    ) -> Result<Vec<HubTask>, Error> {
        let mut by_id: BTreeMap<i64, HubTask> = BTreeMap::new();
        let mut start = after;
        while start < before {
            let end = (start + seed_span).min(before);
            self.sweep_slice(opts, start, end, page_size, retries, on_page, &mut by_id)?;
            start = end;
        }
        Ok(by_id.into_values().collect())
    }

    #[allow(clippy::too_many_arguments)]
    fn sweep_slice(
        &self,
        opts: &ListTasksOpts,
        after: f64,
        before: f64,
        page_size: i64,
        retries: u32,
        on_page: &mut impl FnMut(&[HubTask]),
        by_id: &mut BTreeMap<i64, HubTask>,
    ) -> Result<(), Error> {
        let slice_opts = ListTasksOpts {
            complete_after: Some(after),
            complete_before: Some(before),
            ..opts.clone()
        };
        let query = QueryOpts {
            limit: Some(page_size),
            ..Default::default()
        };
        let page = crate::retry(retries, || self.list_tasks(&slice_opts, &query))?;
        let full = (page.len() as i64) >= page_size;
        on_page(&page);
        if full && before - after > 1.0 {
            let mid = after + (before - after) / 2.0;
            // Left first, so progress moves chronologically.
            self.sweep_slice(opts, after, mid, page_size, retries, on_page, by_id)?;
            self.sweep_slice(opts, mid, before, page_size, retries, on_page, by_id)?;
            return Ok(());
        }
        if full {
            eprintln!(
                "warning: {page_size}+ tasks completed within one second \
                 around unix {after:.0}; a few may be missed"
            );
        }
        for task in page {
            by_id.insert(task.id, task);
        }
        Ok(())
    }

    /// Walk `listTasks` pages ordered by descending task id (i.e.
    /// newest first), stopping after the page whose oldest task
    /// was created before `min_create_ts`, or on a short page.
    ///
    /// This is the load-proof sweep primitive: measured on a hub
    /// where even a five-minute completion-window filter timed
    /// out, a 500-row `order: -id` page (an index walk, no
    /// completion filter) returned in ~1.3s. Task ids are
    /// assigned at creation, so descending id is descending
    /// create-time; callers window by completion client-side and
    /// pass a `min_create_ts` bound stretched by their maximum
    /// expected task duration. Tasks arriving mid-walk only shift
    /// pages deeper (ids grow monotonically), so the id-keyed
    /// dedupe absorbs overlap and nothing is skipped.
    ///
    /// Each page is retried `retries` times; `on_page` runs per
    /// page for progress output and caller-paced sleeps.
    pub fn walk_tasks_desc(
        &self,
        opts: &ListTasksOpts,
        page_size: i64,
        retries: u32,
        min_create_ts: f64,
        on_page: &mut impl FnMut(&[HubTask]),
    ) -> Result<Vec<HubTask>, Error> {
        let mut by_id: BTreeMap<i64, HubTask> = BTreeMap::new();
        let mut offset = 0i64;
        loop {
            let query = QueryOpts {
                limit: Some(page_size),
                offset: Some(offset),
                order: Some("-id".to_string()),
            };
            let page = crate::retry(retries, || self.list_tasks(opts, &query))?;
            let n = page.len() as i64;
            on_page(&page);
            let oldest_create = page
                .iter()
                .filter_map(|t| t.create_ts)
                .fold(f64::INFINITY, f64::min);
            for task in page {
                by_id.insert(task.id, task);
            }
            if n < page_size || oldest_create < min_create_ts {
                return Ok(by_id.into_values().collect());
            }
            offset += page_size;
        }
    }

    /// `getTaskInfo(task_id, request)` — one task, optionally with
    /// its request decoded.
    pub fn get_task_info(&self, task_id: i64, decode_request: bool) -> Result<HubTask, Error> {
        let result = self.client.call(
            "getTaskInfo",
            &[Value::Int(task_id), Value::Boolean(decode_request)],
        )?;
        task_from_value(&result)
    }

    /// `listHosts()` — builder `(id, name)` pairs.
    pub fn list_hosts(&self) -> Result<Vec<(i64, String)>, Error> {
        self.list_id_names("listHosts", "name")
    }

    /// `listChannels()` — channel `(id, name)` pairs.
    pub fn list_channels(&self) -> Result<Vec<(i64, String)>, Error> {
        self.list_id_names("listChannels", "name")
    }

    fn list_id_names(&self, method: &str, name_key: &str) -> Result<Vec<(i64, String)>, Error> {
        let result = self.client.call(method, &[])?;
        let items = result
            .as_array()
            .ok_or_else(|| Error::Parse(format!("{method} did not return an array")))?;
        items
            .iter()
            .map(|item| {
                let id = item
                    .get("id")
                    .and_then(Value::as_int)
                    .ok_or_else(|| Error::Parse(format!("{method}: entry without id")))?;
                let name = item
                    .get(name_key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Parse(format!("{method}: entry without name")))?
                    .to_string();
                Ok((id, name))
            })
            .collect()
    }
}

impl ListTasksOpts {
    fn to_value(&self) -> Value {
        let mut map = HashMap::new();
        if let Some(method) = &self.method {
            map.insert("method".to_string(), Value::String(method.clone()));
        }
        if let Some(ts) = self.complete_after {
            map.insert("completeAfter".to_string(), Value::Double(ts));
        }
        if let Some(ts) = self.complete_before {
            map.insert("completeBefore".to_string(), Value::Double(ts));
        }
        if let Some(parents) = &self.parent {
            map.insert(
                "parent".to_string(),
                Value::Array(parents.iter().map(|&p| Value::Int(p)).collect()),
            );
        }
        if self.decode {
            map.insert("decode".to_string(), Value::Boolean(true));
        }
        Value::Struct(map)
    }
}

impl QueryOpts {
    fn to_value(&self) -> Value {
        let mut map = HashMap::new();
        if let Some(limit) = self.limit {
            map.insert("limit".to_string(), Value::Int(limit));
        }
        if let Some(offset) = self.offset {
            map.insert("offset".to_string(), Value::Int(offset));
        }
        if let Some(order) = &self.order {
            map.insert("order".to_string(), Value::String(order.clone()));
        }
        Value::Struct(map)
    }
}

/// Decode one task struct. Tolerant: only `id`, `method`, and
/// `state` are required; everything else decodes to `None` when
/// absent, nil, or of an unexpected type.
fn task_from_value(v: &Value) -> Result<HubTask, Error> {
    let id = v
        .get("id")
        .and_then(Value::as_int)
        .ok_or_else(|| Error::Parse("task without id".to_string()))?;
    let method = v
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Parse(format!("task {id} without method")))?
        .to_string();
    let state = v
        .get("state")
        .and_then(Value::as_int)
        .ok_or_else(|| Error::Parse(format!("task {id} without state")))?;
    let as_f64 = |key: &str| match v.get(key) {
        Some(Value::Double(d)) => Some(*d),
        Some(Value::Int(i)) => Some(*i as f64),
        _ => None,
    };
    Ok(HubTask {
        id,
        parent: v.get("parent").and_then(Value::as_int),
        method,
        arch: v.get("arch").and_then(Value::as_str).map(str::to_string),
        state,
        create_ts: as_f64("create_ts"),
        start_ts: as_f64("start_ts"),
        completion_ts: as_f64("completion_ts"),
        host_id: v.get("host_id").and_then(Value::as_int),
        channel_id: v.get("channel_id").and_then(Value::as_int),
        owner: v.get("owner").and_then(Value::as_int),
        owner_name: v
            .get("owner_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        priority: v.get("priority").and_then(Value::as_int),
        weight: as_f64("weight"),
        request: v.get("request").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wiremock XML-RPC tests follow cpu-sig-tracker's pattern for
    /// blocking clients: start the mock server on a runtime, call
    /// the blocking client from the test thread.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn task_xml(id: i64, arch: &str, completion: Option<f64>) -> String {
        let completion = match completion {
            Some(ts) => format!("<value><double>{ts}</double></value>"),
            None => "<value><nil/></value>".to_string(),
        };
        format!(
            "<value><struct>\
             <member><name>id</name><value><i8>{id}</i8></value></member>\
             <member><name>parent</name><value><int>77</int></value></member>\
             <member><name>method</name><value><string>buildArch</string></value></member>\
             <member><name>arch</name><value><string>{arch}</string></value></member>\
             <member><name>state</name><value><int>2</int></value></member>\
             <member><name>create_ts</name><value><double>1000.5</double></value></member>\
             <member><name>start_ts</name><value><double>1060.5</double></value></member>\
             <member><name>completion_ts</name>{completion}</member>\
             <member><name>host_id</name><value><int>643</int></value></member>\
             <member><name>owner_name</name><value><string>alice</string></value></member>\
             <member><name>request</name><value><array><data>\
             <value><string>tasks/1/2/foo-1.0-1.fc45.src.rpm</string></value>\
             </data></array></value></member>\
             </struct></value>"
        )
    }

    fn response(inner: &str) -> String {
        format!(
            "<?xml version='1.0'?><methodResponse><params><param>\
             <value><array><data>{inner}</data></array></value>\
             </param></params></methodResponse>"
        )
    }

    #[test]
    fn list_tasks_decodes_typed_fields() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = block_on(MockServer::start());
        let body = response(&format!(
            "{}{}",
            task_xml(5_000_000_000, "s390x", Some(2000.0)),
            task_xml(43, "ppc64le", None)
        ));
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .and(body_string_contains("<methodName>listTasks</methodName>"))
                .and(body_string_contains("completeAfter"))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server),
        );

        let hub = HubClient::new(&server.uri());
        let opts = ListTasksOpts {
            method: Some("buildArch".to_string()),
            complete_after: Some(900.0),
            decode: true,
            ..Default::default()
        };
        let tasks = hub.list_tasks(&opts, &QueryOpts::default()).unwrap();
        assert_eq!(tasks.len(), 2);
        // i8-encoded id beyond i32 range decodes.
        assert_eq!(tasks[0].id, 5_000_000_000);
        assert_eq!(tasks[0].arch.as_deref(), Some("s390x"));
        assert_eq!(tasks[0].parent, Some(77));
        assert_eq!(tasks[0].completion_ts, Some(2000.0));
        assert!(tasks[0].request.is_some());
        // Nil completion decodes as None.
        assert_eq!(tasks[1].completion_ts, None);
        assert_eq!(tasks[1].owner_name.as_deref(), Some("alice"));
    }

    #[test]
    fn sweep_bisects_full_slices_and_dedupes_boundaries() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = block_on(MockServer::start());
        // Request 1 (full window [0, 1000]): a full page (3 =
        // page_size) -> bisect. Request 2 (left half, fetched
        // first) and request 3 (right half) come back short; task
        // 2 completed exactly at the midpoint and appears in BOTH
        // halves — the dedupe must collapse it. Mounts are
        // consulted in order and expire via up_to_n_times.
        let full_window = response(&format!(
            "{}{}{}",
            task_xml(1, "x86_64", Some(400.0)),
            task_xml(2, "s390x", Some(500.0)),
            task_xml(3, "aarch64", Some(900.0))
        ));
        let left = response(&format!(
            "{}{}",
            task_xml(1, "x86_64", Some(400.0)),
            task_xml(2, "s390x", Some(500.0))
        ));
        let right = response(&format!(
            "{}{}",
            task_xml(2, "s390x", Some(500.0)),
            task_xml(3, "aarch64", Some(900.0))
        ));
        for body in [full_window, left] {
            block_on(
                Mock::given(method("POST"))
                    .and(path("/"))
                    .and(body_string_contains("listTasks"))
                    .respond_with(ResponseTemplate::new(200).set_body_string(body))
                    .up_to_n_times(1)
                    .mount(&server),
            );
        }
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .and(body_string_contains("listTasks"))
                .respond_with(ResponseTemplate::new(200).set_body_string(right))
                .mount(&server),
        );

        let hub = HubClient::new(&server.uri());
        let mut slices = 0;
        let tasks = hub
            .sweep_completion_window(
                &ListTasksOpts::default(),
                0.0,
                1000.0,
                3,
                0,
                f64::INFINITY,
                &mut |_| slices += 1,
            )
            .unwrap();
        assert_eq!(slices, 3);
        // Deduped across the slice boundary, ordered by id.
        let ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn walk_desc_stops_at_create_bound_and_dedupes_shifted_pages() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = block_on(MockServer::start());
        // Page 1: full (2 = page_size), newest tasks, created well
        // after the bound. Page 2: a task that also appeared on
        // page 1 (new arrivals shifted the pages) plus one created
        // BEFORE the bound -> the walk stops after this page.
        fn task_with_create(id: i64, create: f64) -> String {
            format!(
                "<value><struct>\
                 <member><name>id</name><value><int>{id}</int></value></member>\
                 <member><name>method</name><value><string>build</string></value></member>\
                 <member><name>state</name><value><int>2</int></value></member>\
                 <member><name>create_ts</name><value><double>{create}</double></value></member>\
                 </struct></value>"
            )
        }
        let page1 = response(&format!(
            "{}{}",
            task_with_create(30, 3000.0),
            task_with_create(20, 2000.0)
        ));
        let page2 = response(&format!(
            "{}{}",
            task_with_create(20, 2000.0),
            task_with_create(10, 500.0)
        ));
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .and(body_string_contains("listTasks"))
                .and(body_string_contains("-id"))
                .respond_with(ResponseTemplate::new(200).set_body_string(page1))
                .up_to_n_times(1)
                .mount(&server),
        );
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .and(body_string_contains("listTasks"))
                .respond_with(ResponseTemplate::new(200).set_body_string(page2))
                .expect(1)
                .mount(&server),
        );

        let hub = HubClient::new(&server.uri());
        let mut pages = 0;
        let tasks = hub
            .walk_tasks_desc(&ListTasksOpts::default(), 2, 0, 1000.0, &mut |_| pages += 1)
            .unwrap();
        // Page 2 was full too, but its oldest create_ts (500) is
        // below the bound (1000) -> no page 3 (the expect(1) on
        // the second mount enforces it).
        assert_eq!(pages, 2);
        let ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![10, 20, 30]);
    }

    #[test]
    fn fault_is_error_and_not_retriable() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = block_on(MockServer::start());
        let fault = "<?xml version='1.0'?><methodResponse><fault>\
             <value><struct>\
             <member><name>faultCode</name><value><int>1000</int></value></member>\
             <member><name>faultString</name><value><string>invalid method</string></value></member>\
             </struct></value></fault></methodResponse>";
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(200).set_body_string(fault))
                .mount(&server),
        );

        let hub = HubClient::new(&server.uri());
        let err = hub
            .list_tasks(&ListTasksOpts::default(), &QueryOpts::default())
            .unwrap_err();
        assert!(!err.is_retriable(), "faults must not be retried: {err}");
    }

    #[test]
    fn retry_recovers_from_transient_5xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = block_on(MockServer::start());
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(502))
                .up_to_n_times(1)
                .mount(&server),
        );
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_string(response(&task_xml(
                        9,
                        "x86_64",
                        Some(1.0),
                    ))),
                )
                .mount(&server),
        );

        let hub = HubClient::new(&server.uri());
        let tasks = crate::retry(2, || {
            hub.list_tasks(&ListTasksOpts::default(), &QueryOpts::default())
        })
        .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, 9);
    }

    #[test]
    fn list_hosts_maps_ids_to_names() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = block_on(MockServer::start());
        let body = "<?xml version='1.0'?><methodResponse><params><param>\
             <value><array><data>\
             <value><struct>\
             <member><name>id</name><value><int>643</int></value></member>\
             <member><name>name</name><value><string>buildvm-ppc64le-01.iad2</string></value></member>\
             </struct></value>\
             </data></array></value></param></params></methodResponse>";
        block_on(
            Mock::given(method("POST"))
                .and(path("/"))
                .and(body_string_contains("listHosts"))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server),
        );

        let hub = HubClient::new(&server.uri());
        let hosts = hub.list_hosts().unwrap();
        assert_eq!(hosts, vec![(643, "buildvm-ppc64le-01.iad2".to_string())]);
    }
}
