# sandogasa-closure

Dependency-closure walking over fedrq, with a persisted graph and an
offline oracle. Extracted from `poi-tracker deps`, where it drives the
inventory-culling workflow; the same engine is what `ebranch resolve`
needs for the source side of a branch-request closure.

- **`PkgQuery`** — the backend seam: subpackages of sources, sources
  with their BuildRequires, providers of capabilities (each with its
  own Requires, so a wave's answer seeds the next). Implemented for
  `sandogasa_fedrq::Fedrq` and for `GraphBackedQuery`.
- **`walk`** — breadth-first over binary packages, one batched query
  per wave, parallel chunks for large waves; base-repo providers
  terminate, repos of interest collect, `src:` entries carry
  BuildRequires, and an optional fixpoint turns newly collected owned
  packages into roots. Returns a report (first attributions) and a
  `DepsGraph` (every edge).
- **`DepsGraph`** — the persisted graph: `reachable`, `dependents`,
  `witness_reason`, `merge`; serde round-trips it as JSON.
- **`GraphBackedQuery`** — answers `providers()` from a saved graph
  for capabilities it knows (their Requires reconstructed from the
  recorded edges) and asks the inner query only for the frontier.
  Roots always expand live, since a package the graph met as a
  dependency carries only the feature subpackages something requested.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
