//! The person a card's work is done for (migration 0170, `issues.requester`).
//!
//! A cell pushes and opens pull requests with the GitHub sign-in of whoever
//! asked for the work, so this value decides whose account is used. It is
//! therefore stamped by the server from what it verified about the caller,
//! never read from a request body, and the database refuses to change it once
//! set (trigger `issues_requester_immutable`).
//!
//! Who asks, and what the card gets:
//! - a verified member (Google sign-in, dashboard or MCP): `member:<email>`;
//! - the owner (proven owner credential): `AMUX_OWNER_REQUESTER`, else `owner`;
//! - a worker: the requester of the card that worker is working on now, when
//!   exactly one person is behind its `doing` cards. A lead splitting a goal
//!   into child cards therefore files them for the person who set the goal;
//! - an automated policy, or a worker with no single answer: nobody (NULL).
//!   Nothing is pushed for such a card until a person asks for it.
//!
//! Children filed with an `epic` take the parent's requester in the database
//! (trigger `issues_requester_from_epic`), which runs first and wins.

use axum::http::HeaderMap;
use rusqlite::{params, Connection, OptionalExtension};

/// Env var naming the owner in requester fields, e.g. `member:ceo@example.com`.
pub const OWNER_REQUESTER_ENV: &str = "AMUX_OWNER_REQUESTER";

/// Where a new card's requester comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A person the server verified on this request.
    Person(String),
    /// A worker: inherit from the cards it is working on now.
    FromLane(String),
    /// Nobody can be named.
    Unknown,
}

/// Decide the source from the verified request. `worker_session` is the
/// verified worker header (empty for humans).
pub fn source_for_create(headers: &HeaderMap, worker_session: &str) -> Source {
    match super::human_proof::proven(headers) {
        Some(super::human_proof::Proven::Policy(_)) => Source::Unknown,
        Some(super::human_proof::Proven::Member(m)) => person(&m),
        Some(super::human_proof::Proven::Owner) => Source::Person(owner_label()),
        None => {
            let lane = worker_session.trim();
            if lane.is_empty() {
                Source::Unknown
            } else {
                Source::FromLane(lane.to_string())
            }
        }
    }
}

fn person(name: &str) -> Source {
    let name = name.trim();
    if name.is_empty() {
        Source::Unknown
    } else {
        Source::Person(name.to_string())
    }
}

/// The owner's requester value.
pub fn owner_label() -> String {
    std::env::var(OWNER_REQUESTER_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "owner".to_string())
}

/// The single person behind a worker's `doing` cards, if there is exactly one.
pub fn active_lane_requester(conn: &Connection, lane: &str) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT requester FROM issues \
          WHERE session = ?1 AND status = 'doing' AND requester IS NOT NULL \
            AND deleted IS NULL AND COALESCE(archived, 0) = 0 \
          LIMIT 2",
    )?;
    let found: Vec<String> = stmt
        .query_map(params![lane], |r| r.get::<_, String>(0))?
        .flatten()
        .collect();
    Ok(match found.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    })
}

/// Stamp a just-created card. A value the epic trigger already set is kept.
/// Returns what the card ends up with.
pub fn stamp(conn: &Connection, id: &str, source: &Source) -> rusqlite::Result<Option<String>> {
    let value = match source {
        Source::Person(p) => Some(p.clone()),
        Source::FromLane(lane) => active_lane_requester(conn, lane)?,
        Source::Unknown => None,
    };
    if let Some(v) = value {
        conn.execute(
            "UPDATE issues SET requester = ?1 WHERE id = ?2 AND requester IS NULL",
            params![v, id],
        )?;
    }
    read(conn, id)
}

/// The card's requester, if any.
pub fn read(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    Ok(conn
        .query_row("SELECT requester FROM issues WHERE id = ?1", params![id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        migrate::apply_all(&mut c).unwrap();
        c
    }

    fn card(c: &Connection, id: &str, session: &str, status: &str, epic: Option<&str>) {
        c.execute(
            "INSERT INTO issues (id, title, status, session, epic, created, updated) \
             VALUES (?1, ?1, ?2, ?3, ?4, 1, 1)",
            params![id, status, session, epic],
        )
        .unwrap();
    }

    #[test]
    fn a_person_is_stamped_as_verified() {
        let c = conn();
        card(&c, "G-1", "lead", "todo", None);
        let got = stamp(&c, "G-1", &Source::Person("member:a@x.com".into())).unwrap();
        assert_eq!(got.as_deref(), Some("member:a@x.com"));
    }

    #[test]
    fn a_worker_card_inherits_the_person_behind_its_doing_card() {
        let c = conn();
        card(&c, "G-1", "lead", "doing", None);
        stamp(&c, "G-1", &Source::Person("member:a@x.com".into())).unwrap();
        card(&c, "C-1", "dev", "todo", None);
        let got = stamp(&c, "C-1", &Source::FromLane("lead".into())).unwrap();
        assert_eq!(got.as_deref(), Some("member:a@x.com"));
    }

    #[test]
    fn two_people_behind_a_lane_means_nobody() {
        let c = conn();
        card(&c, "G-1", "lead", "doing", None);
        card(&c, "G-2", "lead", "doing", None);
        stamp(&c, "G-1", &Source::Person("member:a@x.com".into())).unwrap();
        stamp(&c, "G-2", &Source::Person("member:b@x.com".into())).unwrap();
        card(&c, "C-1", "dev", "todo", None);
        assert_eq!(stamp(&c, "C-1", &Source::FromLane("lead".into())).unwrap(), None);
    }

    #[test]
    fn a_lane_with_nothing_in_progress_names_nobody() {
        let c = conn();
        card(&c, "G-1", "lead", "done", None);
        stamp(&c, "G-1", &Source::Person("member:a@x.com".into())).unwrap();
        card(&c, "C-1", "dev", "todo", None);
        assert_eq!(stamp(&c, "C-1", &Source::FromLane("lead".into())).unwrap(), None);
    }

    #[test]
    fn a_child_with_an_epic_takes_the_parent_requester_first() {
        let c = conn();
        card(&c, "G-1", "lead", "todo", None);
        stamp(&c, "G-1", &Source::Person("member:a@x.com".into())).unwrap();
        card(&c, "C-1", "dev", "todo", Some("G-1"));
        assert_eq!(read(&c, "C-1").unwrap().as_deref(), Some("member:a@x.com"));
        // A later stamp from another source does not replace it.
        let got = stamp(&c, "C-1", &Source::Person("member:b@x.com".into())).unwrap();
        assert_eq!(got.as_deref(), Some("member:a@x.com"));
    }

    #[test]
    fn the_requester_cannot_be_rewritten() {
        let c = conn();
        card(&c, "G-1", "lead", "todo", None);
        stamp(&c, "G-1", &Source::Person("member:a@x.com".into())).unwrap();
        let err = c
            .execute("UPDATE issues SET requester = 'member:b@x.com' WHERE id = 'G-1'", [])
            .unwrap_err();
        assert!(err.to_string().contains("immutable"), "{err}");
        assert!(c.execute("UPDATE issues SET requester = NULL WHERE id = 'G-1'", []).is_err());
        assert_eq!(read(&c, "G-1").unwrap().as_deref(), Some("member:a@x.com"));
    }

    #[test]
    fn other_column_updates_still_work_on_a_stamped_card() {
        let c = conn();
        card(&c, "G-1", "lead", "todo", None);
        stamp(&c, "G-1", &Source::Person("member:a@x.com".into())).unwrap();
        c.execute("UPDATE issues SET status = 'doing', title = 't2' WHERE id = 'G-1'", [])
            .unwrap();
        assert_eq!(read(&c, "G-1").unwrap().as_deref(), Some("member:a@x.com"));
    }

    #[test]
    fn a_worker_without_a_lane_and_a_policy_name_nobody() {
        let h = HeaderMap::new();
        assert_eq!(source_for_create(&h, ""), Source::Unknown);
        assert_eq!(source_for_create(&h, "lead"), Source::FromLane("lead".into()));
    }
}
