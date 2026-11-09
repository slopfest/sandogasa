// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A package checkout's remotes, by the part each plays rather than by
//! name (#102).
//!
//! Three remotes matter: CentOS Stream's repo, a SIG's mirror of it, and
//! the personal fork merge requests to Stream are opened from. Which one
//! is `origin` depends on where the checkout was cloned from: Stream for
//! a Proposed Update started there, the SIG's repo for Hyperscale. So
//! each is found by the project its URL points at, and a remote the
//! tools add is named after its part (`proposed_updates`, `hyperscale`)
//! rather than given a name that says nothing.

use crate::Sig;

/// The GitLab group CentOS Stream's package repos are in.
pub const STREAM_GROUP: &str = "redhat/centos-stream/rpms";

/// The part a remote plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// CentOS Stream's repo of the package.
    Stream,
    /// A SIG's repo of the package.
    Sig(Sig),
    /// A personal fork of Stream's repo, as `centpkg fork` makes it
    /// (`<user>/centos_rpms_<package>`).
    Fork,
}

/// One remote: its name, fetch URL, project path and part.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub path: String,
    pub role: Role,
    pub package: String,
}

/// A checkout's remotes that play a part, in the order git lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Remotes(pub Vec<Remote>);

/// A git remote URL's project path: `CentOS/proposed_updates/rpms/rsync`
/// for `git+ssh://git@gitlab.com/CentOS/proposed_updates/rpms/rsync.git`
/// or `git@gitlab.com:CentOS/…`.
pub fn project_path(url: &str) -> Option<String> {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let path = rest.trim_end_matches('/').trim_end_matches(".git");
    (!path.is_empty()).then(|| path.to_string())
}

/// The part the project at `path` plays, and the package it is of.
pub fn role_of(path: &str) -> Option<(Role, String)> {
    let (group, last) = path.rsplit_once('/')?;
    if last.is_empty() {
        return None;
    }
    if group.eq_ignore_ascii_case(STREAM_GROUP) {
        return Some((Role::Stream, last.to_string()));
    }
    if let Some(sig) = Sig::ALL
        .into_iter()
        .find(|s| group.eq_ignore_ascii_case(s.gitlab_group()))
    {
        return Some((Role::Sig(sig), last.to_string()));
    }
    // A fork is in someone's own namespace, one level down.
    let package = last.strip_prefix("centos_rpms_")?;
    (!group.contains('/') && !package.is_empty()).then(|| (Role::Fork, package.to_string()))
}

/// The name a tool gives a remote it adds for `role`: the SIG's name
/// for a SIG's repo, `stream` for Stream's. A fork is added by
/// `centpkg fork`, which names it after the user.
pub fn name_for(role: Role) -> &'static str {
    match role {
        Role::Stream => "stream",
        Role::Sig(sig) => sig.name(),
        Role::Fork => "fork",
    }
}

impl Remotes {
    /// From `git remote -v` output: each fetch line's remote, kept when
    /// it plays a part.
    pub fn parse(remote_v: &str) -> Self {
        Remotes(
            remote_v
                .lines()
                .filter(|l| l.ends_with("(fetch)"))
                .filter_map(|l| {
                    let mut parts = l.split_whitespace();
                    let (name, url) = (parts.next()?, parts.next()?);
                    let path = project_path(url)?;
                    let (role, package) = role_of(&path)?;
                    Some(Remote {
                        name: name.to_string(),
                        url: url.to_string(),
                        path,
                        role,
                        package,
                    })
                })
                .collect(),
        )
    }

    /// The remote playing `role`, if one does.
    pub fn find(&self, role: Role) -> Option<&Remote> {
        self.0.iter().find(|r| r.role == role)
    }

    /// The package the remotes are for: Stream's, else a SIG's, else the
    /// fork's.
    pub fn package(&self) -> Option<&str> {
        [Role::Stream]
            .into_iter()
            .chain(Sig::ALL.map(Role::Sig))
            .chain([Role::Fork])
            .find_map(|role| self.find(role))
            .map(|r| r.package.as_str())
    }

    /// The SIGs whose repos are remotes.
    pub fn sigs(&self) -> Vec<Sig> {
        Sig::ALL
            .into_iter()
            .filter(|s| self.find(Role::Sig(*s)).is_some())
            .collect()
    }

    /// The name to add a remote for `role` under, or an error when a
    /// remote of that name points elsewhere. `names` is every remote's
    /// name, played part or not (`git remote`).
    pub fn name_to_add(role: Role, names: &[String]) -> Result<&'static str, String> {
        let name = name_for(role);
        if names.iter().any(|n| n == name) {
            return Err(format!(
                "the checkout has a remote `{name}` already, pointing somewhere else: rename or \
                 remove it, or add the {} repo under another name yourself",
                match role {
                    Role::Stream => "CentOS Stream".to_string(),
                    Role::Sig(s) => format!("{} SIG's", s.name()),
                    Role::Fork => "fork's".to_string(),
                }
            ));
        }
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rsync's checkout: Stream as origin, the fork centpkg added, and
    /// the Proposed Updates repo under the name sig-repo once used.
    const RSYNC: &str = "\
michel-slm\tgit+ssh://git@gitlab.com/michel-slm/centos_rpms_rsync.git (fetch)
michel-slm\tgit+ssh://git@gitlab.com/michel-slm/centos_rpms_rsync.git (push)
origin\tgit+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git (fetch)
origin\tgit+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git (push)
sig\tgit+ssh://git@gitlab.com/CentOS/proposed_updates/rpms/rsync.git (fetch)
sig\tgit+ssh://git@gitlab.com/CentOS/proposed_updates/rpms/rsync.git (push)
";

    #[test]
    fn each_remote_is_known_by_the_project_it_points_at() {
        let r = Remotes::parse(RSYNC);
        assert_eq!(r.find(Role::Stream).unwrap().name, "origin");
        assert_eq!(r.find(Role::Fork).unwrap().name, "michel-slm");
        assert_eq!(r.find(Role::Sig(Sig::ProposedUpdates)).unwrap().name, "sig");
        assert_eq!(r.find(Role::Sig(Sig::Hyperscale)), None);
        assert_eq!(r.package(), Some("rsync"));
        assert_eq!(r.sigs(), [Sig::ProposedUpdates]);
    }

    /// A Hyperscale checkout cloned from the SIG's repo, with Stream
    /// added under another name.
    #[test]
    fn origin_can_be_the_sigs_repo() {
        let r = Remotes::parse(
            "origin\tgit@gitlab.com:CentOS/Hyperscale/rpms/systemd.git (fetch)\n\
             stream\thttps://gitlab.com/redhat/centos-stream/rpms/systemd (fetch)\n\
             fedora\thttps://src.fedoraproject.org/rpms/systemd.git (fetch)\n",
        );
        assert_eq!(r.find(Role::Stream).unwrap().name, "stream");
        assert_eq!(r.find(Role::Sig(Sig::Hyperscale)).unwrap().name, "origin");
        assert_eq!(r.package(), Some("systemd"));
        assert_eq!(r.0.len(), 2, "Fedora's dist-git plays no part here");
    }

    #[test]
    fn an_added_remote_is_named_after_its_part() {
        assert_eq!(
            name_for(Role::Sig(Sig::ProposedUpdates)),
            "proposed_updates"
        );
        assert_eq!(name_for(Role::Sig(Sig::Hyperscale)), "hyperscale");
        assert_eq!(name_for(Role::Stream), "stream");
        let names = ["origin".to_string(), "proposed_updates".to_string()];
        let err = Remotes::name_to_add(Role::Sig(Sig::ProposedUpdates), &names).unwrap_err();
        assert!(err.contains("`proposed_updates` already"), "{err}");
        assert_eq!(
            Remotes::name_to_add(Role::Sig(Sig::Hyperscale), &names),
            Ok("hyperscale")
        );
    }

    #[test]
    fn a_fork_is_one_namespace_down() {
        assert_eq!(
            role_of("michel-slm/centos_rpms_rsync"),
            Some((Role::Fork, "rsync".to_string()))
        );
        assert_eq!(role_of("group/sub/centos_rpms_rsync"), None);
        assert_eq!(role_of("redhat/centos-stream/rpms/"), None);
        assert_eq!(project_path("https://gitlab.com/a/b/"), Some("a/b".into()));
    }
}
