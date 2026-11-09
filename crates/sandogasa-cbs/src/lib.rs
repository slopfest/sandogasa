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

pub mod login;

/// The koji profile CBS is reached through, and the `cbs` command
/// that wraps it.
pub const PROFILE: &str = "cbs";

/// The SIGs whose conventions are known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sig {
    /// The Hyperscale SIG.
    Hyperscale,
    /// The CentOS Proposed Updates SIG.
    ProposedUpdates,
}

/// A stage a build moves through, each a tag of the collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// A collection of `sig`'s packages.
    pub fn new(sig: &str, major: u32, stream: bool, repository: &str) -> Self {
        Self {
            sig: sig.to_string(),
            major,
            stream,
            repository: repository.to_string(),
        }
    }

    /// `9s` or `9`.
    fn release(&self) -> String {
        format!("{}{}", self.major, if self.stream { "s" } else { "" })
    }

    /// `hyperscale9s-packages-main`: what every name below starts with.
    pub fn prefix(&self) -> String {
        format!(
            "{}{}-packages-{}",
            self.sig,
            self.release(),
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
        (self.stream && self.sig == "hyperscale" && self.repository == "facebook")
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
            ("proposed_updates", "main") => Some(format!("centos-stream-{n}-{arch}")),
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

    /// The GitLab group holding the SIG's package repositories, under
    /// `https://gitlab.com/`.
    pub fn gitlab_group(self) -> &'static str {
        match self {
            Sig::Hyperscale => "CentOS/Hyperscale/rpms",
            Sig::ProposedUpdates => "CentOS/proposed_updates/rpms",
        }
    }

    /// The collection a branch of this SIG's package repos builds into.
    ///
    /// Proposed Updates: `c<N>s-proposed-<key>` (one topic branch per
    /// proposed change, named after its first RHEL issue) builds into
    /// `main`.
    ///
    /// Hyperscale, by the branch's suffix after `c<N>s-`:
    ///
    /// | Suffix | Repository |
    /// |---|---|
    /// | `hs`, `sig-hyperscale` | `main` |
    /// | `sig-hyperscale-v<N>` (an older release, for emergency builds) | `main` |
    /// | `hs+asahi` | `asahi` |
    /// | `hs+fb` | `facebook` |
    /// | `hsk` | `kernel` |
    /// | `hsx`, `sig-hyperscale-experimental` | `experimental` |
    /// | `sig-hyperscale-spin` | `spin` |
    ///
    /// Anything else is an error naming the branch, rather than a guess.
    pub fn collection_for_branch(self, branch: &str) -> Result<Collection, String> {
        let unknown = || {
            format!(
                "{branch} is not a {} branch CBS builds from (expected {})",
                self.name(),
                match self {
                    Sig::Hyperscale => "c<N>s-hs, -hsx, -hs+fb, -hs+asahi or -hsk",
                    Sig::ProposedUpdates => "c<N>s-proposed-<RHEL key>",
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
                "hs" | "sig-hyperscale" => Some("main"),
                // systemd's c10s-sig-hyperscale-v257 and the like: older
                // releases kept for emergency builds, into main.
                v if v
                    .strip_prefix("sig-hyperscale-v")
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) =>
                {
                    Some("main")
                }
                "hs+asahi" => Some("asahi"),
                "hs+fb" => Some("facebook"),
                "hsk" => Some("kernel"),
                "hsx" | "sig-hyperscale-experimental" => Some("experimental"),
                "sig-hyperscale-spin" => Some("spin"),
                _ => None,
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
