// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fedora's shared meeting rooms, from the free-text locations calendar
//! entries give.
//!
//! A meeting's location is free text, so one room appears under many
//! names: `meeting-1@chat.fedoraproject.org`,
//! `https://matrix.to/#/%23meeting-1:fedoraproject.org`,
//! `#meeting-1:fedoraproject.org`, and the IRC channel it replaced,
//! `fedora-meeting-1@irc.libera.chat`. [`Room::from_location`] reads them
//! all as the Matrix room. A video call link, a city or a free-form note
//! is not a shared room.

/// A shared meeting room: `meeting` or `meeting-N`, a Matrix room on
/// fedoraproject.org.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Room {
    /// `meeting`, `meeting-1`, ...
    pub name: String,
}

impl Room {
    /// The room `name` names: `meeting-1`, `#meeting-1`, or any location
    /// form [`Room::from_location`] reads.
    pub fn parse(name: &str) -> Option<Room> {
        Room::from_location(name).or_else(|| room_name(name.trim().trim_start_matches('#')))
    }

    /// The shared room a calendar entry's location names, if any.
    pub fn from_location(location: &str) -> Option<Room> {
        let loc = location.trim().to_ascii_lowercase();
        // matrix.to links, with the `#` escaped or not.
        let loc = loc.replace("%23", "#");
        let loc = loc
            .split_once("matrix.to/#/")
            .map_or(loc.as_str(), |(_, rest)| rest)
            .to_string();
        // `#name:fedoraproject.org`, a Matrix alias.
        if let Some(alias) = loc.strip_prefix('#')
            && let Some((name, server)) = alias.split_once(':')
            && is_fedora_server(server)
        {
            return room_name(name);
        }
        // `name@chat.fedoraproject.org`, `name@fedoraproject.org`, or the
        // legacy IRC `fedora-name@irc.libera.chat`.
        let (name, server) = loc.split_once('@')?;
        let name = name.trim_start_matches('#');
        if is_fedora_server(server) {
            return room_name(name);
        }
        if server.contains("libera") || server.contains("freenode") {
            return room_name(name.strip_prefix("fedora-")?);
        }
        None
    }

    /// The Matrix alias: `#meeting-1:fedoraproject.org`.
    pub fn alias(&self) -> String {
        format!("#{}:fedoraproject.org", self.name)
    }
}

impl std::fmt::Display for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// Whether `server` is Fedora's chat or its Matrix server name, with
/// whatever follows it in the location (`?via=...` on a matrix.to link,
/// a path, a note) cut off.
fn is_fedora_server(server: &str) -> bool {
    let server = server
        .split(|c: char| c.is_whitespace() || matches!(c, '?' | '/' | '(' | ','))
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    matches!(
        server,
        "fedoraproject.org" | "chat.fedoraproject.org" | "matrix.fedoraproject.org"
    )
}

/// `meeting` or `meeting-N` as a room; anything else is not one.
fn room_name(name: &str) -> Option<Room> {
    let ok = name == "meeting"
        || name
            .strip_prefix("meeting-")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    ok.then(|| Room {
        name: name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(location: &str) -> Option<String> {
        Room::from_location(location).map(|r| r.name)
    }

    #[test]
    fn every_name_of_a_room_reads_as_it() {
        for location in [
            "meeting-1@chat.fedoraproject.org",
            "https://matrix.to/#/#meeting-1:fedoraproject.org",
            "https://matrix.to/#/%23meeting-1:fedoraproject.org",
            "#meeting-1:fedoraproject.org",
            "fedora-meeting-1@irc.libera.chat",
            "#fedora-meeting-1@libera",
            "Meeting-1@Chat.FedoraProject.org",
            "https://matrix.to/#/#meeting-1:fedoraproject.org?via=fedoraproject.org",
            "meeting-1@chat.fedoraproject.org (Matrix)",
            "meeting-1@chat.fedoraproject.org.",
            "#meeting-1:fedoraproject.org, video on request",
        ] {
            assert_eq!(room(location).as_deref(), Some("meeting-1"), "{location}");
        }
        assert_eq!(
            room("meeting@fedoraproject.org").as_deref(),
            Some("meeting")
        );
        assert_eq!(
            room("fedora-meeting@irc.libera.chat").as_deref(),
            Some("meeting")
        );
    }

    #[test]
    fn other_places_are_not_rooms() {
        for location in [
            "",
            "https://meet.google.com/hfy-jpoo-muq",
            "Bangalore",
            "#anaconda:fedoraproject.org",
            "atomic@libera",
            "fedora-qa@irc.libera.chat",
            "#meeting-1:matrix.org",
            "meeting-x@chat.fedoraproject.org",
        ] {
            assert_eq!(room(location), None, "{location}");
        }
    }

    #[test]
    fn a_room_is_named_by_its_short_name_too() {
        assert_eq!(Room::parse("meeting-3").unwrap().name, "meeting-3");
        assert_eq!(
            Room::parse("#meeting").unwrap().alias(),
            "#meeting:fedoraproject.org"
        );
        assert_eq!(
            Room::parse("meeting-2@chat.fedoraproject.org")
                .unwrap()
                .to_string(),
            "meeting-2"
        );
        assert!(Room::parse("anaconda").is_none());
    }
}
