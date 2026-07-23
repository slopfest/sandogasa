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
  tree; retriable-vs-fault `Error` classification; `retry(n, f)`
  with exponential backoff at the crate root.
- `hub::HubClient` — typed helpers:
  - `list_tasks(opts, query)` / `list_tasks_paged(opts, page_size,
    on_page)` — `listTasks` with method/completion-window filters
    and decoded requests; the paged variant advances the offset
    until a short page, ordered by task id (`on_page` is the
    caller's hook for progress output and polite inter-page
    sleeps).
  - `get_task_info(task_id, decode_request)`.
  - `list_hosts()` / `list_channels()` — id → name pairs.
- `hub::HubTask` — Option-tolerant task record: timestamps are the
  hub's UTC unix doubles (`create_ts`/`start_ts`/`completion_ts`;
  queue wait = start − create, build time = completion − start).

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
