// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The CentOS Build System (CBS) conventions the SIG tools share: how a
//! SIG's dist-git branch names the collection it builds into, the
//! collection's targets and tags, the GitLab group its package repos
//! live in, the mock config that matches it, and the login CBS's
//! `cbs` koji profile needs.
//!
//! A collection's names in CBS all derive from one prefix,
//! `<sig><major>[s]-packages-<repository>`:
//!
//! - build target: `hyperscale9s-packages-main-el9s`, and its buildroot
//!   tag `hyperscale9s-packages-main-el9s-build`;
//! - the tags a build moves through: `-candidate` (where the target
//!   puts it), `-testing`, `-release`.
//!
//! Each SIG has its own branch convention: Proposed Updates proposes a
//! change on `c10s-proposed-RHEL-170526`, Hyperscale builds its main
//! repository from `c10s-hs` (see [`Sig::collection_for_branch`]).
//!
//! A SIG's release package, `centos-release-<sig>`, which installs the
//! SIG's repository files, is the exception: it is built from the SIG's
//! repo but into Extras' common collection, `extras<major>s-extras-common`
//! (see [`release_package_collection`]).

pub mod login;
pub mod lookaside;
pub mod packages;
pub mod remotes;
#[cfg(test)]
mod test_support;
pub mod tracking;

/// The koji profile CBS is reached through, and the `cbs` command
/// that wraps it.
pub const PROFILE: &str = "cbs";

/// The merge methods GitLab offers a project, as its API names them:
/// a merge commit, a merge commit after a rebase, a fast-forward.
pub const MERGE_METHODS: [&str; 3] = ["ff", "merge", "rebase_merge"];

/// The SIGs whose conventions are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sig {
    /// The Hyperscale SIG.
    Hyperscale,
    /// The CentOS Proposed Updates SIG.
    ProposedUpdates,
}

/// A stage a build moves through, each a tag of the collection, in
/// order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Where the build target puts a new build.
    Candidate,
    /// Built and offered for testing.
    Testing,
    /// Released to the SIG's repository.
    Release,
}

/// One SIG collection in CBS: a SIG's packages for one CentOS major
/// release, in one repository (`main`, `experimental`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Collection {
    /// The SIG's name in CBS: `hyperscale`, `proposed_updates`.
    pub sig: String,
    /// The CentOS major release: 9, 10.
    pub major: u32,
    /// Whether it builds for CentOS Stream (`9s`) rather than the
    /// release (`9`).
    pub stream: bool,
    /// The repository within the SIG: `main`, `experimental`,
    /// `facebook`, ….
    pub repository: String,
}

impl Stage {
    /// The stage a build is promoted to from this one, if any.
    pub fn next(self) -> Option<Stage> {
        match self {
            Stage::Candidate => Some(Stage::Testing),
            Stage::Testing => Some(Stage::Release),
            Stage::Release => None,
        }
    }

    /// The tag suffix: `candidate`, `testing`, `release`.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Candidate => "candidate",
            Stage::Testing => "testing",
            Stage::Release => "release",
        }
    }
}

impl Collection {
    /// A collection of `sig`'s packages. Extras' common collection, which
    /// is named differently, is [`Collection::extras_common`].
    pub fn new(sig: &str, major: u32, stream: bool, repository: &str) -> Self {
        Self {
            sig: sig.to_string(),
            major,
            stream,
            repository: repository.to_string(),
        }
    }

    /// Extras' common collection for CentOS Stream `major`, which SIGs'
    /// release packages are built into: `extras9s-extras-common`.
    pub fn extras_common(major: u32) -> Self {
        Self {
            sig: "extras".to_string(),
            major,
            stream: true,
            repository: "common".to_string(),
        }
    }

    /// Whether this is Extras' common collection rather than a SIG's.
    pub fn is_extras(&self) -> bool {
        self.sig == "extras"
    }

    /// What the collection holds, between the release and the
    /// repository in its names: `extras` for Extras', `packages` for a
    /// SIG's.
    fn area(&self) -> &'static str {
        if self.is_extras() {
            "extras"
        } else {
            "packages"
        }
    }

    /// The collection and stage a tag is: `hyperscale9s-packages-main-testing`
    /// is Hyperscale's EL9 Stream main collection, in testing, and
    /// `extras10s-extras-common-testing` Extras' common collection, which
    /// SIGs' release packages go through. `None` for any other tag.
    pub fn from_tag(tag: &str) -> Option<(Collection, Stage)> {
        let (rest, stage) = [Stage::Candidate, Stage::Testing, Stage::Release]
            .into_iter()
            .find_map(|s| Some((tag.strip_suffix(&format!("-{}", s.as_str()))?, s)))?;
        if let Some(major) = rest
            .strip_prefix("extras")
            .and_then(|r| r.strip_suffix("s-extras-common"))
            .and_then(|d| d.parse().ok())
        {
            return Some((Collection::extras_common(major), stage));
        }
        let sig = Sig::ALL.into_iter().find(|s| rest.starts_with(s.name()))?;
        let (release, repository) = rest[sig.name().len()..].split_once("-packages-")?;
        let (digits, stream) = match release.strip_suffix('s') {
            Some(d) => (d, true),
            None => (release, false),
        };
        let major = digits.parse().ok()?;
        (!repository.is_empty() && !repository.contains('-')).then(|| {
            (
                Collection::new(sig.name(), major, stream, repository),
                stage,
            )
        })
    }

    /// `9s` or `9`.
    fn release(&self) -> String {
        format!("{}{}", self.major, if self.stream { "s" } else { "" })
    }

    /// `hyperscale9s-packages-main`: what every name below starts with.
    pub fn prefix(&self) -> String {
        format!(
            "{}{}-{}-{}",
            self.sig,
            self.release(),
            self.area(),
            self.repository
        )
    }

    /// The tag for `stage`: `hyperscale9s-packages-main-testing`.
    pub fn tag(&self, stage: Stage) -> String {
        format!("{}-{}", self.prefix(), stage.as_str())
    }

    /// The build target: `hyperscale9s-packages-main-el9s`.
    pub fn target(&self) -> String {
        format!("{}-el{}", self.prefix(), self.release())
    }

    /// The target's buildroot tag: `hyperscale9s-packages-main-el9s-build`.
    pub fn build_tag(&self) -> String {
        format!("{}-build", self.target())
    }

    /// The collection built for RHEL from the same branch, if there is
    /// one: Hyperscale's Facebook repository also builds its Stream
    /// branches (`c9s-hs+fb`) for the RHEL release
    /// (`hyperscale9-packages-facebook-el9`), for systems that deploy
    /// RHEL snapshots, months behind Stream and with an older
    /// toolchain.
    pub fn rhel_variant(&self) -> Option<Collection> {
        (self.stream
            && self.sig == Sig::Hyperscale.name()
            && hyperscale_variant(&self.repository).is_some_and(|v| v.rhel))
        .then(|| Collection::new(&self.sig, self.major, false, &self.repository))
    }

    /// The mock config (shipped in mock-core-configs and
    /// mock-centos-sig-configs) whose buildroot matches this
    /// collection's, for `arch`; `None` when mock ships none, as for
    /// Hyperscale's asahi and kernel repositories, or a non-Stream
    /// release.
    pub fn mock_config(&self, arch: &str) -> Option<String> {
        if !self.stream {
            return None;
        }
        let n = self.major;
        match (self.sig.as_str(), self.repository.as_str()) {
            ("proposed_updates", "main") | ("extras", "common") => {
                Some(format!("centos-stream-{n}-{arch}"))
            }
            ("hyperscale", "main") => Some(format!("centos-stream-hyperscale-{n}-{arch}")),
            ("hyperscale", repo @ ("experimental" | "facebook" | "spin")) => {
                Some(format!("centos-stream-hyperscale-{repo}-{n}-{arch}"))
            }
            _ => None,
        }
    }
}

impl Sig {
    /// Every SIG whose conventions are known.
    pub const ALL: [Sig; 2] = [Sig::Hyperscale, Sig::ProposedUpdates];

    /// The SIG's name in CBS tags: `hyperscale`, `proposed_updates`.
    pub fn name(self) -> &'static str {
        match self {
            Sig::Hyperscale => "hyperscale",
            Sig::ProposedUpdates => "proposed_updates",
        }
    }

    /// The SIG's release package, which installs its repository files:
    /// `centos-release-hyperscale`.
    pub fn release_package(self) -> String {
        format!("centos-release-{}", self.name())
    }

    /// The CentOS Stream major a branch of the SIG's repo is the SIG's
    /// own for, as a candidate for the repo's default branch: Proposed
    /// Updates' `c<N>s-proposed-<key>`, Hyperscale's main repository
    /// branches `c<N>s-hs` and `c<N>s-sig-hyperscale`. `None` for any
    /// other branch: Stream's mirrored `c<N>s`, Hyperscale's other
    /// repositories and older releases kept for emergency builds.
    pub fn own_branch_major(self, branch: &str) -> Option<u32> {
        let (major, suffix) = stream_branch(branch)?;
        let own = match self {
            Sig::ProposedUpdates => self.collection_for_branch(branch).is_ok(),
            Sig::Hyperscale => matches!(suffix, "hs" | "sig-hyperscale"),
        };
        own.then_some(major)
    }

    /// The branch a SIG repo should open on: of the SIG's own branches
    /// ([`Sig::own_branch_major`]), the newest Stream release's, and of
    /// those the one with the newest head commit. `branches` pairs each
    /// branch with its head commit's time; `None` when no branch is the
    /// SIG's own yet.
    pub fn newest_own_branch<T: Ord>(self, branches: &[(String, T)]) -> Option<&str> {
        branches
            .iter()
            .filter_map(|(name, at)| Some(((self.own_branch_major(name)?, at), name.as_str())))
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, name)| name)
    }

    /// The GitLab group holding the SIG's package repositories, under
    /// `https://gitlab.com/`.
    pub fn gitlab_group(self) -> &'static str {
        match self {
            Sig::Hyperscale => "CentOS/Hyperscale/rpms",
            Sig::ProposedUpdates => "CentOS/proposed_updates/rpms",
        }
    }

    /// How merge requests into the SIG's package repos land, as
    /// GitLab's `merge_method` names it, when the SIG has settled on
    /// one; `None` would leave each repo's as it is. Tools let their
    /// config override this per SIG.
    ///
    /// Both SIGs fast-forward (`ff`). Hyperscale's so merge requests are
    /// rebased and history stays linear. Proposed Updates rarely merges
    /// anything: its branches are the Stream merge request's commits,
    /// pushed as they are. But a merge commit in one would part it from
    /// the merge request's head, which `cbspkg sig-repo` compares it
    /// with and pushes onto, and in a mirrored `c<N>s` branch would stop
    /// the mirror updating it; a fast-forward adds only commits the
    /// merge request's own branch has.
    pub fn merge_method(self) -> Option<&'static str> {
        match self {
            Sig::Hyperscale | Sig::ProposedUpdates => Some("ff"),
        }
    }

    /// The merge method the SIG's repos should have: `configured`, an
    /// override from cbspkg's config (`[sigs.<name>] merge_method`, which
    /// hs-relmon reads too, so the two tools agree), else the SIG's own
    /// ([`Sig::merge_method`]). A method GitLab does not know is an
    /// error naming the ones it does.
    pub fn wanted_merge_method(self, configured: Option<&str>) -> Result<Option<String>, String> {
        match configured {
            None => Ok(self.merge_method().map(str::to_string)),
            Some(m) if MERGE_METHODS.contains(&m) => Ok(Some(m.to_string())),
            Some(m) => Err(format!(
                "[sigs.{}] merge_method = \"{m}\": GitLab knows {}",
                self.name(),
                MERGE_METHODS.join(", ")
            )),
        }
    }

    /// The collection a branch of this SIG's package repos builds into.
    ///
    /// Proposed Updates: `c<N>s-proposed-<key>` (one topic branch per
    /// proposed change, named after its first RHEL issue) builds into
    /// `main`.
    ///
    /// Hyperscale, by the branch's suffix after `c<N>s-`: a variant's
    /// ([`HYPERSCALE_VARIANTS`], or the legacy `sig-hyperscale`,
    /// `sig-hyperscale-experimental` and `sig-hyperscale-spin`), or
    /// `sig-hyperscale-v<N>`, an older release kept for emergency builds,
    /// into `main`.
    ///
    /// Anything else is an error naming the branch, rather than a guess.
    pub fn collection_for_branch(self, branch: &str) -> Result<Collection, String> {
        let unknown = || {
            format!(
                "{branch} is not a {} branch CBS builds from (expected {})",
                self.name(),
                match self {
                    Sig::Hyperscale => hyperscale_branches(),
                    Sig::ProposedUpdates => "c<N>s-proposed-<RHEL key>".to_string(),
                }
            )
        };
        let (major, suffix) = stream_branch(branch).ok_or_else(unknown)?;
        let repository = match self {
            Sig::ProposedUpdates => suffix
                .strip_prefix("proposed-")
                .filter(|key| !key.is_empty())
                .map(|_| "main"),
            Sig::Hyperscale => match suffix {
                // systemd's c10s-sig-hyperscale-v257 and the like: older
                // releases kept for emergency builds, into main.
                v if v
                    .strip_prefix("sig-hyperscale-v")
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) =>
                {
                    Some("main")
                }
                "sig-hyperscale" => Some("main"),
                "sig-hyperscale-experimental" => Some("experimental"),
                "sig-hyperscale-spin" => Some("spin"),
                _ => HYPERSCALE_VARIANTS
                    .iter()
                    .find(|v| v.suffix == suffix)
                    .map(|v| v.name),
            },
        }
        .ok_or_else(unknown)?;
        Ok(Collection::new(self.name(), major, true, repository))
    }

    /// The SIG and collection a branch belongs to, trying each known
    /// SIG; `None` for a branch none of them builds from, such as
    /// Stream's own `c10s`.
    pub fn detect(branch: &str) -> Option<(Sig, Collection)> {
        Sig::ALL
            .into_iter()
            .find_map(|sig| Some((sig, sig.collection_for_branch(branch).ok()?)))
    }
}

/// The GitLab project path of a package's CentOS Stream dist-git repo,
/// which every SIG's repo for it mirrors.
pub fn stream_project(package: &str) -> String {
    format!("redhat/centos-stream/rpms/{package}")
}

/// One of Hyperscale's variants, as its docs name them
/// (`policies/branches.md`): the repository it builds into, and the
/// suffix of the branches built for it (`c10s-hs+fb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Variant {
    /// The repository: `main`, `facebook`, ….
    pub name: &'static str,
    /// The branch suffix: `hs`, `hs+fb`, ….
    pub suffix: &'static str,
    /// Still built, but on its way out.
    pub deprecated: bool,
    /// Also built against RHEL rather than CentOS Stream
    /// (`hyperscale9-packages-facebook-el9`).
    pub rhel: bool,
}

const fn variant(
    name: &'static str,
    suffix: &'static str,
    deprecated: bool,
    rhel: bool,
) -> Variant {
    Variant {
        name,
        suffix,
        deprecated,
        rhel,
    }
}

/// Hyperscale's variants, as its docs list them on 2026-11-10, and as
/// CBS has targets for. `hotfixes` is a destination tag only, built
/// through `main`, so it is not one.
pub const HYPERSCALE_VARIANTS: &[Variant] = &[
    variant("main", "hs", false, false),
    variant("spin", "hs-spin", false, false),
    variant("experimental", "hsx", false, false),
    variant("kernel", "hsk", false, false),
    variant("asahi", "hs+asahi", false, false),
    variant("gnome", "hs+gnome", true, false),
    variant("facebook", "hs+fb", false, true),
    variant("intel", "hs+intel", true, false),
];

/// The branches Hyperscale builds from, for an error naming them:
/// `c<N>s-hs, -hs-spin, …`.
pub fn hyperscale_branches() -> String {
    let suffixes: Vec<String> = HYPERSCALE_VARIANTS
        .iter()
        .map(|v| format!("-{}", v.suffix))
        .collect();
    format!("c<N>s{}", suffixes.join(", "))
}

/// The Hyperscale variant `name` is: its name (`facebook`) or its
/// branch suffix (`hs+fb`).
pub fn hyperscale_variant(name: &str) -> Option<Variant> {
    HYPERSCALE_VARIANTS
        .iter()
        .copied()
        .find(|v| v.name == name || v.suffix == name)
}

/// The collections for building into Hyperscale's `stream` variants on
/// CentOS Stream `major`, and its `rhel` variants against RHEL, in that
/// order: `(["main"], ["facebook"])` for 9 is `hyperscale9s-packages-main`
/// and `hyperscale9-packages-facebook`. Each is a name or a branch
/// suffix ([`hyperscale_variant`]); an unknown one, or a RHEL one with
/// no RHEL target, is an error listing what there is.
pub fn hyperscale_collections(
    major: u32,
    stream: &[String],
    rhel: &[String],
) -> Result<Vec<Collection>, String> {
    let names = |pick: fn(&Variant) -> bool| {
        HYPERSCALE_VARIANTS
            .iter()
            .filter(|v| pick(v))
            .map(|v| v.name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out: Vec<Collection> = Vec::new();
    for (wanted, on_stream) in [(stream, true), (rhel, false)] {
        for w in wanted {
            let v = hyperscale_variant(w).ok_or_else(|| {
                format!(
                    "{w} is not a Hyperscale variant (there are {})",
                    names(|_| true)
                )
            })?;
            if !on_stream && !v.rhel {
                return Err(format!(
                    "Hyperscale builds {} for CentOS Stream only; against RHEL there is {}",
                    v.name,
                    names(|v| v.rhel)
                ));
            }
            let c = Collection::new(Sig::Hyperscale.name(), major, on_stream, v.name);
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

/// What SIGs' release packages are named: `centos-release-hyperscale`,
/// `centos-release-ceph-reef`, `centos-release-samba418`.
pub const RELEASE_PACKAGE_PREFIX: &str = "centos-release-";

/// The collection a SIG's release package is built into from a branch
/// of the SIG's repo (`c9s-proposed`, `c10s-sig-hyperscale`): Extras'
/// common collection for the branch's CentOS Stream release, as
/// `centos-release-hyperscale-10-5.el10s` was. Every SIG's release
/// package goes there, whatever it is named after. `None` for any other
/// package, or a branch naming no Stream release.
pub fn release_package_collection(package: &str, branch: &str) -> Option<Collection> {
    let name = package.strip_prefix(RELEASE_PACKAGE_PREFIX)?;
    if name.is_empty() {
        return None;
    }
    Some(Collection::extras_common(stream_major(branch)?))
}

/// The CentOS Stream major a branch starts from: 10 for `c10s`,
/// `c10s-proposed-RHEL-1` or `c10s-CVE-2026-119741`.
pub fn stream_major(branch: &str) -> Option<u32> {
    let rest = branch.strip_prefix('c')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let after = rest[digits.len()..].strip_prefix('s')?;
    (after.is_empty() || after.starts_with('-'))
        .then(|| digits.parse().ok())
        .flatten()
}

/// The branch a Proposed Update is carried on in the SIG's repo:
/// `c10s-proposed-RHEL-303721`, for the CentOS Stream branch `release`
/// and the change's first RHEL issue.
pub fn proposed_branch(release: &str, rhel_key: &str) -> String {
    format!("{release}-proposed-{rhel_key}")
}

/// `c10s-hs` → `(10, "hs")`.
fn stream_branch(branch: &str) -> Option<(u32, &str)> {
    let (release, suffix) = branch.strip_prefix('c')?.split_once("s-")?;
    let major = release
        .chars()
        .all(|c| c.is_ascii_digit())
        .then(|| release.parse().ok())??;
    Some((major, suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sigs_fast_forward() {
        assert_eq!(Sig::Hyperscale.merge_method(), Some("ff"));
        assert_eq!(Sig::ProposedUpdates.merge_method(), Some("ff"));
        for sig in Sig::ALL {
            assert!(
                sig.merge_method()
                    .is_none_or(|m| MERGE_METHODS.contains(&m))
            );
        }
    }

    #[test]
    fn a_configured_merge_method_wins_if_gitlab_knows_it() {
        let hs = Sig::Hyperscale;
        assert_eq!(hs.wanted_merge_method(None).unwrap().as_deref(), Some("ff"));
        assert_eq!(
            hs.wanted_merge_method(Some("rebase_merge"))
                .unwrap()
                .as_deref(),
            Some("rebase_merge")
        );
        let err = hs.wanted_merge_method(Some("squash")).unwrap_err();
        assert_eq!(
            err,
            "[sigs.hyperscale] merge_method = \"squash\": GitLab knows ff, merge, rebase_merge"
        );
    }

    #[test]
    fn a_collection_names_its_target_and_tags_as_cbs_does() {
        // As `koji -p cbs list-targets` showed them on 2026-11-09.
        let pu = Collection::new("proposed_updates", 10, true, "main");
        assert_eq!(pu.target(), "proposed_updates10s-packages-main-el10s");
        assert_eq!(
            pu.build_tag(),
            "proposed_updates10s-packages-main-el10s-build"
        );
        assert_eq!(
            pu.tag(Stage::Candidate),
            "proposed_updates10s-packages-main-candidate"
        );
        assert_eq!(
            pu.tag(Stage::Testing),
            "proposed_updates10s-packages-main-testing"
        );
        assert_eq!(
            pu.tag(Stage::Release),
            "proposed_updates10s-packages-main-release"
        );
        let fb = Collection::new("hyperscale", 10, false, "facebook");
        assert_eq!(fb.target(), "hyperscale10-packages-facebook-el10");
    }

    #[test]
    fn a_stage_tag_names_its_collection_and_stage() {
        let (c, stage) = Collection::from_tag("hyperscale9s-packages-main-testing").unwrap();
        assert_eq!(stage, Stage::Testing);
        assert_eq!(c, Collection::new("hyperscale", 9, true, "main"));
        let (c, stage) = Collection::from_tag("hyperscale9-packages-facebook-release").unwrap();
        assert_eq!(
            (c.stream, c.repository.as_str(), stage),
            (false, "facebook", Stage::Release)
        );
        let (c, _) = Collection::from_tag("proposed_updates10s-packages-main-candidate").unwrap();
        assert_eq!(
            c.tag(Stage::Testing),
            "proposed_updates10s-packages-main-testing"
        );
        let (c, stage) = Collection::from_tag("extras10s-extras-common-testing").unwrap();
        assert_eq!((c, stage), (Collection::extras_common(10), Stage::Testing));
        for not_one in [
            "extras10s-extras-common-el10s-build",
            "extrasXs-extras-common-testing",
            "hyperscale9s-packages-main-el9s-build",
            "aie10s-packages-nvidia-testing",
            "hyperscale9s-images-main-testing",
            "hyperscaleX-packages-main-testing",
        ] {
            assert_eq!(Collection::from_tag(not_one), None, "{not_one}");
        }
    }

    /// As `koji -p cbs buildinfo` showed them on 2026-11-10:
    /// centos-release-proposed_updates-9-1.el9s (task 4531268) and
    /// centos-release-hyperscale-10-5.el10s (task 5428832).
    #[test]
    fn a_sigs_release_package_builds_into_extras_common() {
        let c =
            release_package_collection("centos-release-proposed_updates", "c9s-proposed").unwrap();
        assert_eq!(c.target(), "extras9s-extras-common-el9s");
        assert_eq!(c.tag(Stage::Testing), "extras9s-extras-common-testing");
        assert!(c.is_extras());
        assert_eq!(
            c.mock_config("x86_64").as_deref(),
            Some("centos-stream-9-x86_64")
        );
        assert_eq!(c.rhel_variant(), None);
        let hs =
            release_package_collection("centos-release-hyperscale", "c10s-sig-hyperscale").unwrap();
        assert_eq!(hs.target(), "extras10s-extras-common-el10s");
        // Release packages need not be named after their SIG: Extras'
        // tags held centos-release-ceph-reef and -samba418 on 2026-11-10.
        assert_eq!(
            release_package_collection("centos-release-ceph-reef", "c10s"),
            Some(Collection::extras_common(10))
        );
        for (package, branch) in [
            ("rsync", "c9s-proposed"),
            ("centos-release-", "c9s-proposed"),
            ("centos-release-hyperscale", "main"),
            ("centos-release-hyperscale", "c10x-sig-hyperscale"),
            ("centos-release-hyperscale", "c+10s"),
        ] {
            assert_eq!(
                release_package_collection(package, branch),
                None,
                "{package} {branch}"
            );
        }
        assert!(!Collection::new("proposed_updates", 9, true, "main").is_extras());
        assert_eq!(stream_major("c10s-CVE-2026-1"), Some(10));
        assert_eq!(stream_major("c9s"), Some(9));
        for not_one in ["c9sx", "c+9s", "cs", "9s", "c9"] {
            assert_eq!(stream_major(not_one), None, "{not_one}");
        }
    }

    /// rsync's SIG repo on 2026-11-10, with the stray branches it had
    /// then, and a second c10s proposal made up to stand for the next
    /// CVE.
    #[test]
    fn a_sig_repo_opens_on_its_newest_own_branch() {
        let b = |name: &str, at: u32| (name.to_string(), at);
        let rsync = [
            b("RHEL-169141", 9),
            b("c10s", 8),
            b("c10s-proposed-RHEL-303721", 5),
            b("c11s", 9),
            b("c9s", 9),
            b("c9s-proposed-RHEL-303761", 7),
            b("centos_rpms_rsync-automated-package-update-RHEL-174950", 9),
        ];
        let pu = Sig::ProposedUpdates;
        assert_eq!(
            pu.newest_own_branch(&rsync),
            Some("c10s-proposed-RHEL-303721")
        );
        let mut next_cve = rsync.to_vec();
        next_cve.push(b("c10s-proposed-RHEL-400000", 6));
        assert_eq!(
            pu.newest_own_branch(&next_cve),
            Some("c10s-proposed-RHEL-400000")
        );
        // A newer release wins over a newer commit.
        next_cve.push(b("c9s-proposed-RHEL-400001", 99));
        assert_eq!(
            pu.newest_own_branch(&next_cve),
            Some("c10s-proposed-RHEL-400000")
        );
        assert_eq!(pu.newest_own_branch(&[b("c10s", 1), b("main", 2)]), None);
        let hs = Sig::Hyperscale;
        let systemd = [
            b("c10s", 9),
            b("c10s-sig-hyperscale-v257", 9),
            b("c10s-hs+fb", 9),
            b("c9s-sig-hyperscale", 3),
            b("c10s-sig-hyperscale", 2),
        ];
        assert_eq!(hs.newest_own_branch(&systemd), Some("c10s-sig-hyperscale"));
        assert_eq!(hs.own_branch_major("c10s-hs"), Some(10));
        assert_eq!(hs.own_branch_major("c10s-hsx"), None);
    }

    /// perf-7.3~rc2-1, built from c9s-hs into main on Stream and
    /// facebook on RHEL (`koji -p cbs list-tagged`, 2026-11-10).
    #[test]
    fn hyperscale_variants_are_chosen_by_name_or_branch_suffix() {
        let c = hyperscale_collections(9, &["main".into()], &["facebook".into()]).unwrap();
        assert_eq!(
            c.iter().map(Collection::target).collect::<Vec<_>>(),
            [
                "hyperscale9s-packages-main-el9s",
                "hyperscale9-packages-facebook-el9"
            ]
        );
        let c = hyperscale_collections(10, &["hs+fb".into(), "facebook".into()], &[]).unwrap();
        assert_eq!(c.len(), 1, "the same variant once");
        assert_eq!(c[0].target(), "hyperscale10s-packages-facebook-el10s");
        let err = hyperscale_collections(9, &[], &["main".into()]).unwrap_err();
        assert!(
            err.contains("CentOS Stream only") && err.contains("facebook"),
            "{err}"
        );
        let err = hyperscale_collections(9, &["hotfixes".into()], &[]).unwrap_err();
        assert!(
            err.contains("not a Hyperscale variant") && err.contains("intel"),
            "{err}"
        );
        assert!(hyperscale_variant("hs+intel").unwrap().deprecated);
        assert!(hyperscale_variant("gnome").unwrap().deprecated);
        assert!(!hyperscale_variant("main").unwrap().deprecated);
        let branches = hyperscale_branches();
        assert!(
            branches.starts_with("c<N>s-hs, -hs-spin,") && branches.contains("-hs+intel"),
            "{branches}"
        );
        // Facebook alone is also built against RHEL, by its branch too.
        let fb = Sig::Hyperscale.collection_for_branch("c9s-hs+fb").unwrap();
        assert_eq!(
            fb.rhel_variant().unwrap().target(),
            "hyperscale9-packages-facebook-el9"
        );
        let main = Sig::Hyperscale.collection_for_branch("c9s-hs").unwrap();
        assert_eq!(main.rhel_variant(), None);
    }

    #[test]
    fn proposed_updates_builds_its_topic_branches_into_main() {
        let c = Sig::ProposedUpdates
            .collection_for_branch("c10s-proposed-RHEL-170526")
            .unwrap();
        assert_eq!(c, Collection::new("proposed_updates", 10, true, "main"));
        for not_ours in [
            "c10s",
            "c10s-proposed-",
            "c9s-hs",
            "rhughes/c10s-cve-2026-41651",
        ] {
            let err = Sig::ProposedUpdates
                .collection_for_branch(not_ours)
                .unwrap_err();
            assert!(
                err.contains(not_ours) && err.contains("c<N>s-proposed-"),
                "{err}"
            );
        }
    }

    #[test]
    fn hyperscale_branches_name_their_repository() {
        // Branch names as the Hyperscale repos (rpm, mesa, btrfs-progs,
        // kernel, systemd) carried them on 2026-11-09.
        for (branch, repository) in [
            ("c10s-hs", "main"),
            ("c9s-sig-hyperscale", "main"),
            ("c10s-hs+asahi", "asahi"),
            ("c9s-hs+fb", "facebook"),
            ("c10s-hsk", "kernel"),
            ("c9s-hsx", "experimental"),
            ("c9s-sig-hyperscale-experimental", "experimental"),
            ("c8s-sig-hyperscale-spin", "spin"),
            ("c10s-hs-spin", "spin"),
            ("c9s-hs+gnome", "gnome"),
            ("c9s-hs+intel", "intel"),
        ] {
            let c = Sig::Hyperscale.collection_for_branch(branch).unwrap();
            assert_eq!(c.repository, repository, "{branch}");
            assert!(c.stream, "{branch}");
        }
        assert_eq!(
            Sig::Hyperscale
                .collection_for_branch("c9s-hs+fb")
                .unwrap()
                .target(),
            "hyperscale9s-packages-facebook-el9s"
        );
        assert_eq!(
            Sig::Hyperscale
                .collection_for_branch("c10s-sig-hyperscale-v257")
                .unwrap()
                .repository,
            "main"
        );
        for unknown in [
            "c10s-sig-hyperscale-v",
            "c10s-sig-hyperscale-vX",
            "c10s-hsy",
        ] {
            let err = Sig::Hyperscale.collection_for_branch(unknown).unwrap_err();
            assert!(err.contains(unknown), "{err}");
        }
    }

    #[test]
    fn a_proposed_branch_maps_back_to_its_collection() {
        let branch = proposed_branch("c10s", "RHEL-303721");
        assert_eq!(branch, "c10s-proposed-RHEL-303721");
        assert_eq!(
            Sig::ProposedUpdates
                .collection_for_branch(&branch)
                .unwrap()
                .target(),
            "proposed_updates10s-packages-main-el10s"
        );
        assert_eq!(stream_project("rsync"), "redhat/centos-stream/rpms/rsync");
    }

    #[test]
    fn a_branch_is_matched_to_its_sig() {
        let (sig, c) = Sig::detect("c9s-proposed-RHEL-170528").unwrap();
        assert_eq!(sig, Sig::ProposedUpdates);
        assert_eq!(c.major, 9);
        assert_eq!(Sig::detect("c10s-hsx").unwrap().0, Sig::Hyperscale);
        assert_eq!(Sig::detect("c10s"), None);
        assert_eq!(Sig::detect("main"), None);
        assert_eq!(Sig::detect("cXs-hs"), None);
    }

    #[test]
    fn facebook_branches_also_build_for_rhel() {
        let fb = Sig::Hyperscale.collection_for_branch("c9s-hs+fb").unwrap();
        let rhel = fb.rhel_variant().unwrap();
        assert_eq!(rhel.target(), "hyperscale9-packages-facebook-el9");
        assert_eq!(
            rhel.tag(Stage::Release),
            "hyperscale9-packages-facebook-release"
        );
        assert_eq!(rhel.rhel_variant(), None);
        let main = Sig::Hyperscale.collection_for_branch("c9s-hs").unwrap();
        assert_eq!(main.rhel_variant(), None);
    }

    #[test]
    fn the_mock_config_matches_the_collection_where_mock_ships_one() {
        let mock = |sig: Sig, branch: &str| {
            sig.collection_for_branch(branch)
                .unwrap()
                .mock_config("x86_64")
        };
        assert_eq!(
            mock(Sig::ProposedUpdates, "c10s-proposed-RHEL-1").as_deref(),
            Some("centos-stream-10-x86_64")
        );
        assert_eq!(
            mock(Sig::Hyperscale, "c9s-hs").as_deref(),
            Some("centos-stream-hyperscale-9-x86_64")
        );
        assert_eq!(
            mock(Sig::Hyperscale, "c10s-hs+fb").as_deref(),
            Some("centos-stream-hyperscale-facebook-10-x86_64")
        );
        assert_eq!(
            mock(Sig::Hyperscale, "c9s-hsx").as_deref(),
            Some("centos-stream-hyperscale-experimental-9-x86_64")
        );
        assert_eq!(mock(Sig::Hyperscale, "c10s-hsk"), None);
        assert_eq!(
            Collection::new("hyperscale", 10, false, "facebook").mock_config("x86_64"),
            None
        );
    }

    #[test]
    fn the_sigs_repos_live_in_their_gitlab_groups() {
        assert_eq!(Sig::Hyperscale.gitlab_group(), "CentOS/Hyperscale/rpms");
        assert_eq!(
            Sig::ProposedUpdates.gitlab_group(),
            "CentOS/proposed_updates/rpms"
        );
    }
}
