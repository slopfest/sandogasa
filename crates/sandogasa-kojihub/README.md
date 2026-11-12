# sandogasa-kojihub

Koji hub XML-RPC client for the sandogasa workspace.

Koji's hub speaks XML-RPC (there is no JSON API), so this crate
carries a minimal client: the `xmlrpc` module is the wire layer (a
`Value` tree over blocking reqwest + quick-xml, extracted from
[koji-diff](../../tools/koji-diff/)), and the `hub` module is a
typed layer over the hub methods the sandogasa tools use.

Anonymous calls work for all the read methods here — no Koji
credentials and no `koji` CLI required (contrast with
[sandogasa-koji](../sandogasa-koji/), which wraps the CLI).

## API

- `xmlrpc::Client` — `call(method, params)` returning a `Value`
  tree, and `multicall(calls)`, several calls in one request through
  the hub's `multiCall`, each answered with its result or its fault;
  retriable-vs-fault `Error` classification; `retry(n, f)` with
  exponential backoff at the crate root.
- `hub::HubClient` — typed helpers:
  - `list_tasks(opts, query)` / `list_tasks_paged(opts, page_size,
    on_page)` — `listTasks` with method/completion-window filters
    and decoded requests; the paged variant advances the offset
    until a short page, ordered by task id (`on_page` is the
    caller's hook for progress output and polite inter-page
    sleeps).
  - `get_task_info(task_id, decode_request)`.
  - `list_hosts()` / `list_channels()` — id → name pairs.
  - `builds_rpm_nvras(nvrs)` — each build's binary RPMs as NVRAs,
    through one `multiCall` of `listBuildRPMs`.
  - `builds_state(nvrs)` — each build's state (`hub::BUILD_COMPLETE`
    and the other `BUILD_*` values), or none for a build the hub does
    not know, through one `multiCall` of `getBuild`.
  - `rpms_deps(nvras, dep_type)` — each RPM's requires, provides,
    obsoletes or conflicts (`RpmDeps`, name and version), through one
    `multiCall` of `getRPMDeps`.
  - `latest_builds(tag, packages)` — each package's newest build in a
    tag (NVR and completion time), following inheritance, through one
    `multiCall` of `getLatestBuilds`.
  - `tag_created(tag)` — when a tag was created, from its
    configuration history (`queryHistory`).
  - `packages_first_listed(packages)` — when each package was first
    added to any tag's package list, through one `multiCall` of
    `queryHistory`: the day it entered the distribution.
- `config::hub_url(profile)` — the hub a `koji` CLI profile names,
  read from the CLI's own configuration (`/etc/koji.conf.d/*.conf`,
  `/etc/koji.conf`, `~/.koji/config.d/*.conf`, `~/.koji/config`, the
  last to define it winning), for a tool that takes a
  `--koji-profile` but calls the hub directly.
- `hub::HubTask` — Option-tolerant task record: timestamps are the
  hub's UTC unix doubles (`create_ts`/`start_ts`/`completion_ts`;
  queue wait = start − create, build time = completion − start).

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
