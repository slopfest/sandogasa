// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fetching the files a `sources` file names, for `srpm` and
//! `upload-sources` alike: from the SIG lookaside, which CBS builds
//! from, or, for a package new to the SIG, from CentOS Stream's or
//! Fedora's, where `centpkg sources` and `fedpkg sources` fetch (#234).
//! Each file is checked against its sha512 before it takes its name,
//! which is what makes any lookaside's copy the right one.

use std::path::Path;

use sandogasa_cbs::lookaside::{Source, rpms_url, url};
use sandogasa_cli::explain::Ui;

/// How many source files are fetched at once.
const PARALLEL_FETCHES: usize = 4;

/// The lookasides a source file is fetched from, in the order they are
/// asked.
#[derive(Debug, Clone)]
pub(crate) struct Lookasides {
    /// The SIGs', which CBS builds from.
    pub(crate) sig: String,
    /// CentOS Stream's, where `centpkg sources` fetches.
    pub(crate) stream: String,
    /// Fedora's, where `fedpkg sources` fetches.
    pub(crate) fedora: String,
}

impl Lookasides {
    /// The real ones, or what `CBSPKG_*LOOKASIDE_BASE` point at.
    pub(crate) fn from_env() -> Self {
        Lookasides {
            sig: crate::upload_sources::lookaside_base(),
            stream: crate::upload_sources::stream_lookaside_base(),
            fedora: crate::upload_sources::fedora_lookaside_base(),
        }
    }

    /// Where each lookaside keeps `source`, and its name, in order.
    fn addresses(&self, package: &str, source: &Source) -> [(Origin, String); 3] {
        [
            (Origin::Sig, url(&self.sig, package, source)),
            (Origin::Stream, rpms_url(&self.stream, package, source)),
            (Origin::Fedora, rpms_url(&self.fedora, package, source)),
        ]
    }
}

/// Where a fetched source file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Sig,
    Stream,
    Fedora,
}

impl Origin {
    fn describe(self) -> &'static str {
        match self {
            Origin::Sig => "the SIG lookaside",
            Origin::Stream => "CentOS Stream's lookaside",
            Origin::Fedora => "Fedora's lookaside",
        }
    }
}

/// Fetch `source` into `repo`: from the SIG lookaside, or, when it
/// answers 404, from CentOS Stream's, then Fedora's. Any answer from
/// the SIG lookaside but 200 or 404 is an error, as it was before the
/// others were asked. Streamed to a file beside it, which takes the
/// name only once its sum is right: a tarball can be hundreds of
/// megabytes, and the sum is what makes any lookaside's copy the right
/// one.
fn fetch_one(
    http: &reqwest::blocking::Client,
    repo: &Path,
    source: &Source,
    lookasides: &Lookasides,
    package: &str,
) -> Result<Origin, String> {
    let mut tried = Vec::new();
    let mut found = None;
    for (origin, at) in lookasides.addresses(package, source) {
        let answer = match http.get(&at).send() {
            Ok(resp) if resp.status().is_success() => {
                found = Some((origin, resp));
                break;
            }
            Ok(resp) => {
                let status = resp.status();
                if origin == Origin::Sig && status != reqwest::StatusCode::NOT_FOUND {
                    return Err(format!(
                        "the SIG lookaside answered HTTP {} for {}; upload it with \
                         `cbspkg upload-sources`, or put it in the checkout",
                        status.as_u16(),
                        source.name
                    ));
                }
                format!("HTTP {}", status.as_u16())
            }
            Err(e) if origin == Origin::Sig => {
                return Err(format!("cannot reach the SIG lookaside: {e}"));
            }
            Err(e) => format!("unreachable ({e})"),
        };
        tried.push(format!("{} {answer} at {at}", origin.describe()));
    }
    let Some((origin, mut resp)) = found else {
        return Err(format!(
            "no lookaside has {}: {}; put it in the checkout",
            source.name,
            tried.join(", ")
        ));
    };
    let partial = format!(".{}.part", source.name);
    let written = std::fs::File::create(repo.join(&partial))
        .map_err(|e| e.to_string())
        .and_then(|mut file| resp.copy_to(&mut file).map_err(|e| e.to_string()));
    if let Err(e) = written {
        let _ = std::fs::remove_file(repo.join(&partial));
        return Err(format!("cannot fetch {}: {e}", source.name));
    }
    let candidate = Source {
        name: partial.clone(),
        ..source.clone()
    };
    if !crate::upload_sources::matches(repo, &candidate) {
        let _ = std::fs::remove_file(repo.join(&partial));
        return Err(format!(
            "{} from {} does not match its sha512 sum in `sources`",
            source.name,
            origin.describe()
        ));
    }
    std::fs::rename(repo.join(&partial), repo.join(&source.name)).map_err(|e| e.to_string())?;
    Ok(origin)
}

/// Fetch whichever of `sources` the checkout at `repo` lacks, a few at
/// a time, from the first of `lookasides` that has each. A file the
/// checkout has with another sum is the packager's, perhaps
/// regenerated: an error, never overwritten. With `remind`, files found
/// outside the SIG lookaside are named at the end with the command that
/// uploads them, even when another file then fails, since a file kept
/// in the checkout is not fetched, or named, again. Nothing is fetched
/// in a dry run.
pub(crate) fn fetch_missing(
    ui: &Ui,
    repo: &Path,
    package: &str,
    sources: &[&Source],
    lookasides: &Lookasides,
    remind: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(other) = sources.iter().find(|s| s.hash != "sha512") {
        return Err(format!(
            "{} is listed with {}; the SIG lookaside keeps sha512 sums",
            other.name, other.hash
        )
        .into());
    }
    let mut missing: Vec<&Source> = Vec::new();
    for &source in sources {
        if !repo.join(&source.name).exists() {
            missing.push(source);
        } else if !crate::upload_sources::matches(repo, source) {
            return Err(format!(
                "{} in the checkout does not match its sha512 sum in `sources`: update \
                 `sources` for it, or remove it to fetch the one `sources` names",
                source.name
            )
            .into());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let curl = |name: &str, at: &str| {
        ["curl", "--fail", "--location", "--output", name, at].map(String::from)
    };
    for (i, step) in [
        format!(
            "Fetch the {} source file(s) the checkout lacks from the SIG lookaside",
            missing.len()
        ),
        "Fetch any the SIG lookaside lacks from CentOS Stream's instead, as \
         `centpkg sources` would"
            .to_string(),
        "Fetch any neither has from Fedora's, as `fedpkg sources` would".to_string(),
    ]
    .iter()
    .enumerate()
    {
        ui.step(step);
        for source in &missing {
            let (_, at) = &lookasides.addresses(package, source)[i];
            ui.show_command(&curl(&source.name, at));
        }
    }
    if ui.dry_run {
        return Ok(());
    }
    if !ui.explain {
        eprintln!("Fetching {} source file(s)…", missing.len());
    }
    let http = sandogasa_cli::http::blocking_builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .build()?;
    let mut elsewhere = Vec::new();
    let mut failed = None;
    for batch in missing.chunks(PARALLEL_FETCHES) {
        let results: Vec<Result<Origin, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|source| {
                    let http = &http;
                    scope.spawn(move || fetch_one(http, repo, source, lookasides, package))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err("a fetch panicked".into())))
                .collect()
        });
        for (source, result) in batch.iter().zip(results) {
            match result {
                Ok(origin) => {
                    eprintln!("{}: from {}", source.name, origin.describe());
                    if origin != Origin::Sig {
                        elsewhere.push(source.name.as_str());
                    }
                }
                Err(e) => {
                    failed.get_or_insert(e);
                }
            }
        }
        // The batch's other files are kept, and named below, before
        // the first failure ends the run.
        if failed.is_some() {
            break;
        }
    }
    if remind && !elsewhere.is_empty() {
        // One file in the sentence; several one per line, so a long
        // list does not run on.
        let (what, it) = crate::upload_sources::lacking(&elsewhere);
        if let [_] = elsewhere.as_slice() {
            eprintln!("note: the SIG lookaside {what}. Upload {it} before building:");
        } else {
            eprintln!("note: the SIG lookaside {what}:");
            for name in &elsewhere {
                eprintln!("    {name}");
            }
            eprintln!("Upload {it} before building:");
        }
        sandogasa_cli::explain::eprint_command(&["cbspkg", "upload-sources"].map(String::from));
    }
    match failed {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}
