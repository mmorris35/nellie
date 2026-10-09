//! Lesson tombstones: pointers left behind when a lesson is deleted.
//!
//! Lessons have no update operation, so retitling one means deleting it and
//! creating a new lesson with a new id. Anything that cited the old id (notes,
//! docs, other lessons) would then find nothing. A tombstone records the old
//! id, optionally the id of the lesson that replaced it, and why, so a lookup
//! by the old id can point at the successor (or say the lesson was deleted)
//! instead of failing silently.
//!
//! Successors can themselves be tombstoned, so lookups follow the chain up to
//! [`MAX_CHAIN_DEPTH`] hops and stop on a cycle. Ids are often cited in
//! shortened form, so prefixes of at least [`MIN_PREFIX_LEN`] characters match
//! in both directions: a tombstone's `old_id` may be a prefix of the requested
//! id, and the requested id may be a prefix of a live id or a tombstone's
//! `old_id`.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::lessons::get_lesson;
use super::lessons_search::retry_on_schema_change;
use super::models::LessonRecord;
use crate::error::StorageError;
use crate::Result;

/// Maximum number of successor hops followed when resolving an id.
pub const MAX_CHAIN_DEPTH: usize = 10;

/// Shortest id prefix accepted as a tombstone `old_id` or successor reference.
pub const MIN_PREFIX_LEN: usize = 8;

/// Most live ids listed for an ambiguous prefix.
pub const MAX_CANDIDATES: usize = 10;

const TOMBSTONE_COLUMNS: &str = "old_id, successor_id, reason, created_at, updated_at";

/// A recorded pointer from a deleted lesson id to its successor, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tombstone {
    /// The deleted lesson's id (or an id prefix).
    pub old_id: String,
    /// The lesson that replaced it; `None` if it was deleted outright.
    pub successor_id: Option<String>,
    /// Free-text explanation, possibly empty.
    pub reason: String,
    /// Unix timestamp when the tombstone was first recorded.
    pub created_at: i64,
    /// Unix timestamp when the successor or reason last changed.
    pub updated_at: i64,
}

/// Why a successor chain did not reach a live lesson.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokenChain {
    /// A tombstone in the chain has no successor.
    NoSuccessor,
    /// The chain reaches an id that is neither live nor tombstoned.
    DeadEnd,
    /// The chain revisits an id.
    Cycle,
    /// The chain is longer than [`MAX_CHAIN_DEPTH`].
    TooDeep,
}

/// Outcome of looking up a lesson id.
#[derive(Debug, Clone)]
pub enum LessonResolution {
    /// The id names a live lesson.
    Live(LessonRecord),
    /// The id was tombstoned and its successor chain ends at a live lesson.
    Moved {
        /// Ids visited after the requested one; the last is the live lesson.
        chain: Vec<String>,
        /// Reason recorded on the first tombstone.
        reason: String,
    },
    /// The id was tombstoned but no live lesson succeeds it.
    Deleted {
        /// Ids visited after the requested one (empty for a plain deletion).
        chain: Vec<String>,
        /// Reason recorded on the first tombstone.
        reason: String,
        /// Why the chain stopped.
        broken: BrokenChain,
    },
    /// The id is a prefix that matches more than one live lesson or
    /// tombstone; refusing to guess.
    Ambiguous {
        /// Live lesson ids the id is a prefix of (at most [`MAX_CANDIDATES`]).
        live: Vec<String>,
        /// Tombstones matching the id as a prefix in either direction.
        tombstones: Vec<Tombstone>,
    },
    /// The id is neither live nor tombstoned.
    NotFound,
}

/// Summary of a tombstone import.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Rows that created a new tombstone.
    pub imported: usize,
    /// Rows that changed an existing tombstone.
    pub updated: usize,
    /// Rows identical to an existing tombstone.
    pub unchanged: usize,
    /// Rows whose `old_id` is still a live lesson (not imported).
    pub skipped_live: Vec<String>,
    /// Imported rows whose successor does not resolve to a live lesson.
    pub unresolved_successor: Vec<String>,
    /// Malformed rows, as `line N: reason`.
    pub invalid: Vec<String>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn db_err(context: &str) -> impl Fn(rusqlite::Error) -> crate::Error + '_ {
    move |e| StorageError::Database(format!("{context}: {e}")).into()
}

fn row_to_tombstone(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tombstone> {
    Ok(Tombstone {
        old_id: row.get(0)?,
        successor_id: row.get(1)?,
        reason: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

/// Fetch the tombstone whose `old_id` is exactly `id`.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn get_tombstone(conn: &Connection, id: &str) -> Result<Option<Tombstone>> {
    conn.query_row(
        &format!("SELECT {TOMBSTONE_COLUMNS} FROM lesson_tombstones WHERE old_id = ?"),
        [id],
        row_to_tombstone,
    )
    .optional()
    .map_err(db_err("failed to get tombstone"))
}

/// Fetch tombstones whose `old_id` is a proper prefix of `id`.
///
/// Only prefixes of at least [`MIN_PREFIX_LEN`] characters count.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn find_prefix_tombstones(conn: &Connection, id: &str) -> Result<Vec<Tombstone>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {TOMBSTONE_COLUMNS}
             FROM lesson_tombstones
             WHERE length(old_id) >= ?1 AND length(old_id) < length(?2)
               AND substr(?2, 1, length(old_id)) = old_id
             ORDER BY old_id"
        ))
        .map_err(db_err("failed to query tombstones"))?;
    let rows = stmt
        .query_map(params![MIN_PREFIX_LEN, id], row_to_tombstone)
        .map_err(db_err("failed to query tombstones"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_err("failed to read tombstone"))
}

/// Fetch tombstones whose `old_id` starts with `prefix` and is longer than it.
///
/// Returns nothing for a prefix shorter than [`MIN_PREFIX_LEN`].
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn find_extending_tombstones(conn: &Connection, prefix: &str) -> Result<Vec<Tombstone>> {
    if prefix.len() < MIN_PREFIX_LEN {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {TOMBSTONE_COLUMNS}
             FROM lesson_tombstones
             WHERE length(old_id) > length(?1) AND substr(old_id, 1, length(?1)) = ?1
             ORDER BY old_id"
        ))
        .map_err(db_err("failed to query tombstones"))?;
    let rows = stmt
        .query_map([prefix], row_to_tombstone)
        .map_err(db_err("failed to query tombstones"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_err("failed to read tombstone"))
}

/// List all tombstones, ordered by `old_id`.
///
/// # Errors
///
/// Returns an error if the database query fails.
pub fn list_tombstones(conn: &Connection) -> Result<Vec<Tombstone>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {TOMBSTONE_COLUMNS} FROM lesson_tombstones ORDER BY old_id"
        ))
        .map_err(db_err("failed to list tombstones"))?;
    let rows = stmt
        .query_map([], row_to_tombstone)
        .map_err(db_err("failed to list tombstones"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_err("failed to read tombstone"))
}

/// Insert or replace a tombstone.
///
/// Returns `None` if an identical tombstone already existed, otherwise
/// `Some(true)` for a new tombstone and `Some(false)` for a changed one.
/// The original `created_at` is kept on update; `updated_at` records the change.
///
/// # Errors
///
/// Returns an error if the database write fails.
pub fn upsert_tombstone(
    conn: &Connection,
    old_id: &str,
    successor_id: Option<&str>,
    reason: &str,
) -> Result<Option<bool>> {
    let existing = get_tombstone(conn, old_id)?;
    if let Some(ref t) = existing {
        if t.successor_id.as_deref() == successor_id && t.reason == reason {
            return Ok(None);
        }
    }
    retry_on_schema_change(conn, || {
        conn.execute(
            "INSERT INTO lesson_tombstones (old_id, successor_id, reason, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(old_id) DO UPDATE SET
                successor_id = excluded.successor_id,
                reason = excluded.reason,
                updated_at = excluded.updated_at",
            params![old_id, successor_id, reason, now()],
        )
    })
    .map_err(db_err("failed to write tombstone"))?;
    Ok(Some(existing.is_none()))
}

fn live_lesson(conn: &Connection, id: &str) -> Result<Option<LessonRecord>> {
    match get_lesson(conn, id) {
        Ok(lesson) => Ok(Some(lesson)),
        Err(crate::Error::Storage(StorageError::NotFound { .. })) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Ids of live lessons starting with `prefix` (at most [`MAX_CANDIDATES`]).
fn live_ids_with_prefix(conn: &Connection, prefix: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT id FROM lessons WHERE substr(id, 1, length(?1)) = ?1 ORDER BY id LIMIT ?2")
        .map_err(db_err("failed to query lessons"))?;
    let rows = stmt
        .query_map(params![prefix, MAX_CANDIDATES], |row| row.get(0))
        .map_err(db_err("failed to query lessons"))?;
    rows.collect::<rusqlite::Result<Vec<String>>>()
        .map_err(db_err("failed to read lesson id"))
}

/// Resolve a reference to the full id of a live lesson, without tombstones.
///
/// Accepts an exact live id, or a prefix of at least [`MIN_PREFIX_LEN`]
/// characters matching exactly one live lesson.
fn live_reference(conn: &Connection, reference: &str) -> Result<Option<String>> {
    if live_lesson(conn, reference)?.is_some() {
        return Ok(Some(reference.to_string()));
    }
    if reference.len() >= MIN_PREFIX_LEN {
        let ids = live_ids_with_prefix(conn, reference)?;
        if ids.len() == 1 {
            return Ok(ids.into_iter().next());
        }
    }
    Ok(None)
}

/// The tombstone that applies to `id`: exact match first, then a unique
/// prefix match in either direction.
enum TombstoneMatch {
    One(Tombstone),
    Many(Vec<Tombstone>),
    None,
}

fn match_tombstone(conn: &Connection, id: &str) -> Result<TombstoneMatch> {
    if let Some(t) = get_tombstone(conn, id)? {
        return Ok(TombstoneMatch::One(t));
    }
    let mut prefixes = find_prefix_tombstones(conn, id)?;
    prefixes.extend(find_extending_tombstones(conn, id)?);
    Ok(match prefixes.len() {
        0 => TombstoneMatch::None,
        1 => TombstoneMatch::One(prefixes.remove(0)),
        _ => TombstoneMatch::Many(prefixes),
    })
}

/// The tombstone to start resolving `id` (not a live id) from, or the final
/// answer when there is none: an exact tombstone first, then a unique prefix
/// match against live ids and tombstones.
fn first_tombstone(
    conn: &Connection,
    id: &str,
) -> Result<std::result::Result<Tombstone, LessonResolution>> {
    if let Some(t) = get_tombstone(conn, id)? {
        return Ok(Ok(t));
    }
    let live = if id.len() >= MIN_PREFIX_LEN {
        live_ids_with_prefix(conn, id)?
    } else {
        Vec::new()
    };
    let mut tombstones = match match_tombstone(conn, id)? {
        TombstoneMatch::One(t) => vec![t],
        TombstoneMatch::Many(ts) => ts,
        TombstoneMatch::None => Vec::new(),
    };
    Ok(match (live.len(), tombstones.len()) {
        (0, 0) => Err(LessonResolution::NotFound),
        (1, 0) => {
            Err(live_lesson(conn, &live[0])?
                .map_or(LessonResolution::NotFound, LessonResolution::Live))
        }
        (0, 1) => Ok(tombstones.remove(0)),
        _ => Err(LessonResolution::Ambiguous { live, tombstones }),
    })
}

/// Look up a lesson id, following tombstones to a live successor.
///
/// An exact live id wins, then an exact tombstone. Otherwise an id of at
/// least [`MIN_PREFIX_LEN`] characters is matched as a prefix against live
/// ids and tombstones (both directions); exactly one match is used, more than
/// one is [`LessonResolution::Ambiguous`]. Successors stored as id prefixes
/// are accepted when they match exactly one live lesson.
///
/// # Errors
///
/// Returns an error if a database query fails.
pub fn resolve_lesson_id(conn: &Connection, id: &str) -> Result<LessonResolution> {
    if let Some(lesson) = live_lesson(conn, id)? {
        return Ok(LessonResolution::Live(lesson));
    }
    let first = match first_tombstone(conn, id)? {
        Ok(t) => t,
        Err(resolution) => return Ok(resolution),
    };

    let reason = first.reason.clone();
    let mut seen: HashSet<String> = HashSet::from([id.to_string(), first.old_id.clone()]);
    let mut chain = Vec::new();
    let mut current = first;
    let deleted = |chain: Vec<String>, broken| LessonResolution::Deleted {
        chain,
        reason: reason.clone(),
        broken,
    };

    loop {
        let Some(next) = current.successor_id.clone() else {
            return Ok(deleted(chain, BrokenChain::NoSuccessor));
        };
        if chain.len() >= MAX_CHAIN_DEPTH {
            return Ok(deleted(chain, BrokenChain::TooDeep));
        }
        let next = live_reference(conn, &next)?.unwrap_or(next);
        if !seen.insert(next.clone()) {
            chain.push(next);
            return Ok(deleted(chain, BrokenChain::Cycle));
        }
        chain.push(next.clone());
        if live_lesson(conn, &next)?.is_some() {
            return Ok(LessonResolution::Moved {
                chain,
                reason: reason.clone(),
            });
        }
        current = match match_tombstone(conn, &next)? {
            TombstoneMatch::One(t) => {
                if t.old_id != next && !seen.insert(t.old_id.clone()) {
                    return Ok(deleted(chain, BrokenChain::Cycle));
                }
                t
            }
            TombstoneMatch::Many(_) | TombstoneMatch::None => {
                return Ok(deleted(chain, BrokenChain::DeadEnd));
            }
        };
    }
}

/// Resolve a successor reference to the full id of a live lesson.
///
/// Accepts a live id, a unique live id prefix, or a tombstoned id whose chain
/// ends at a live lesson. Returns `None` if none of these apply.
///
/// # Errors
///
/// Returns an error if a database query fails.
pub fn resolve_successor(conn: &Connection, reference: &str) -> Result<Option<String>> {
    if let Some(id) = live_reference(conn, reference)? {
        return Ok(Some(id));
    }
    Ok(match resolve_lesson_id(conn, reference)? {
        LessonResolution::Live(lesson) => Some(lesson.id),
        LessonResolution::Moved { mut chain, .. } => chain.pop(),
        _ => None,
    })
}

/// Outcome of [`delete_lesson_with_tombstone`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TombstoneDelete {
    /// The lesson was deleted and a tombstone recorded.
    Deleted {
        /// The resolved successor id stored in the tombstone.
        successor_id: Option<String>,
    },
    /// The successor does not resolve to a live lesson other than the one
    /// being deleted; nothing was changed.
    InvalidSuccessor,
}

/// Delete a lesson and record a tombstone for its id.
///
/// The successor, if given, must resolve (see [`resolve_successor`]) to a live
/// lesson other than the one being deleted; its resolved full id is stored.
/// Run inside a transaction so the delete and the tombstone land together.
///
/// # Errors
///
/// Returns a `NotFound` error if the lesson does not exist, or an error if a
/// database operation fails.
pub fn delete_lesson_with_tombstone(
    conn: &Connection,
    id: &str,
    successor: Option<&str>,
    reason: &str,
) -> Result<TombstoneDelete> {
    let successor_id = match successor {
        Some(reference) => match resolve_successor(conn, reference)? {
            Some(resolved) if resolved != id => Some(resolved),
            _ => return Ok(TombstoneDelete::InvalidSuccessor),
        },
        None => None,
    };
    super::lessons::delete_lesson(conn, id)?;
    upsert_tombstone(conn, id, successor_id.as_deref(), reason)?;
    Ok(TombstoneDelete::Deleted { successor_id })
}

/// Import tombstones from a TSV map with header `old_id<TAB>new_id<TAB>why`.
///
/// Rows whose `old_id` is still a live lesson (exactly, or as a prefix of
/// one) are skipped. A `new_id` that is a unique live id prefix is expanded
/// to the full id; otherwise it is stored as given, and reported if it does
/// not resolve to a live lesson once every row is in. An empty `new_id`
/// records a plain deletion. Re-importing the same map changes nothing.
/// Run inside a transaction.
///
/// # Errors
///
/// Returns an error if the header is missing or a database operation fails.
pub fn import_tombstones(conn: &Connection, tsv: &str) -> Result<ImportReport> {
    let mut lines = tsv.lines().enumerate();
    let header_ok = lines
        .next()
        .map(|(_, h)| h.trim_end_matches('\r').split('\t').collect::<Vec<_>>())
        .is_some_and(|cols| cols.len() >= 2 && cols[0] == "old_id" && cols[1] == "new_id");
    if !header_ok {
        return Err(crate::Error::internal(
            "tombstone map must start with the header old_id<TAB>new_id<TAB>why",
        ));
    }

    let mut report = ImportReport::default();
    let mut with_successor = Vec::new();
    for (index, line) in lines {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let mut cols = line.splitn(3, '\t');
        let old_id = cols.next().unwrap_or("").trim();
        let new_id = cols.next().unwrap_or("").trim();
        let reason = cols.next().unwrap_or("").trim();
        if old_id.is_empty() {
            report
                .invalid
                .push(format!("line {}: empty old_id", index + 1));
            continue;
        }
        if old_id.len() < MIN_PREFIX_LEN {
            report.invalid.push(format!(
                "line {}: old_id shorter than {MIN_PREFIX_LEN} characters",
                index + 1
            ));
            continue;
        }
        if live_lesson(conn, old_id)?.is_some() || !live_ids_with_prefix(conn, old_id)?.is_empty() {
            report.skipped_live.push(old_id.to_string());
            continue;
        }
        let successor = if new_id.is_empty() {
            None
        } else {
            Some(live_reference(conn, new_id)?.unwrap_or_else(|| new_id.to_string()))
        };
        match upsert_tombstone(conn, old_id, successor.as_deref(), reason)? {
            Some(true) => report.imported += 1,
            Some(false) => report.updated += 1,
            None => report.unchanged += 1,
        }
        if let Some(successor) = successor {
            with_successor.push((old_id.to_string(), successor));
        }
    }

    for (old_id, successor) in with_successor {
        if resolve_successor(conn, &successor)?.is_none() {
            report.unresolved_successor.push(old_id);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{insert_lesson, migrate, Database};

    fn setup_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.with_conn(migrate).unwrap();
        db
    }

    fn add_lesson(conn: &Connection, id: &str) {
        let mut lesson = LessonRecord::new(format!("Lesson {id}"), "Content", vec![]);
        lesson.id = id.to_string();
        insert_lesson(conn, &lesson).unwrap();
    }

    #[test]
    fn live_lesson_resolves_to_itself() {
        let db = setup_db();
        db.with_conn(|conn| {
            add_lesson(conn, "live-0001");
            upsert_tombstone(conn, "live-0001", None, "stale")?;
            match resolve_lesson_id(conn, "live-0001")? {
                LessonResolution::Live(l) => assert_eq!(l.id, "live-0001"),
                other => panic!("expected live, got {other:?}"),
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn two_hop_chain_reaches_live_successor() {
        let db = setup_db();
        db.with_conn(|conn| {
            add_lesson(conn, "lesson-c");
            upsert_tombstone(conn, "lesson-a", Some("lesson-b"), "retitled")?;
            upsert_tombstone(conn, "lesson-b", Some("lesson-c"), "folded")?;
            match resolve_lesson_id(conn, "lesson-a")? {
                LessonResolution::Moved { chain, reason } => {
                    assert_eq!(chain, vec!["lesson-b", "lesson-c"]);
                    assert_eq!(reason, "retitled");
                }
                other => panic!("expected moved, got {other:?}"),
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn deletion_and_dead_end_are_deleted() {
        let db = setup_db();
        db.with_conn(|conn| {
            upsert_tombstone(conn, "gone-0001", None, "obsolete")?;
            upsert_tombstone(conn, "gone-0002", Some("nowhere-01"), "")?;
            assert!(matches!(
                resolve_lesson_id(conn, "gone-0001")?,
                LessonResolution::Deleted { broken: BrokenChain::NoSuccessor, ref reason, .. }
                    if reason == "obsolete"
            ));
            assert!(matches!(
                resolve_lesson_id(conn, "gone-0002")?,
                LessonResolution::Deleted {
                    broken: BrokenChain::DeadEnd,
                    ..
                }
            ));
            assert!(matches!(
                resolve_lesson_id(conn, "unknown-01")?,
                LessonResolution::NotFound
            ));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cycle_is_detected() {
        let db = setup_db();
        db.with_conn(|conn| {
            upsert_tombstone(conn, "cycle-aa", Some("cycle-bb"), "")?;
            upsert_tombstone(conn, "cycle-bb", Some("cycle-aa"), "")?;
            upsert_tombstone(conn, "self-loop", Some("self-loop"), "")?;
            for id in ["cycle-aa", "self-loop"] {
                assert!(matches!(
                    resolve_lesson_id(conn, id)?,
                    LessonResolution::Deleted {
                        broken: BrokenChain::Cycle,
                        ..
                    }
                ));
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn overlong_chain_stops() {
        let db = setup_db();
        db.with_conn(|conn| {
            for i in 0..=MAX_CHAIN_DEPTH + 1 {
                let next = format!("hop-{:04}", i + 1);
                upsert_tombstone(conn, &format!("hop-{i:04}"), Some(&next), "")?;
            }
            assert!(matches!(
                resolve_lesson_id(conn, "hop-0000")?,
                LessonResolution::Deleted {
                    broken: BrokenChain::TooDeep,
                    ..
                }
            ));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn prefix_tombstone_matches_and_ambiguity_is_reported() {
        let db = setup_db();
        db.with_conn(|conn| {
            add_lesson(conn, "target-lesson");
            upsert_tombstone(conn, "abcdef12", Some("target-lesson"), "short id")?;
            assert!(matches!(
                resolve_lesson_id(conn, "abcdef12-3456-full")?,
                LessonResolution::Moved { ref chain, .. } if chain == &["target-lesson"]
            ));
            // Shorter than MIN_PREFIX_LEN: never used as a prefix.
            upsert_tombstone(conn, "fedcba9", None, "")?;
            assert!(matches!(
                resolve_lesson_id(conn, "fedcba98-full")?,
                LessonResolution::NotFound
            ));
            upsert_tombstone(conn, "12345678", None, "")?;
            upsert_tombstone(conn, "123456789", None, "")?;
            match resolve_lesson_id(conn, "1234567890-full")? {
                LessonResolution::Ambiguous { tombstones, .. } => assert_eq!(tombstones.len(), 2),
                other => panic!("expected ambiguous, got {other:?}"),
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn delete_records_tombstone_and_validates_successor() {
        let db = setup_db();
        db.with_conn(|conn| {
            add_lesson(conn, "old-lesson");
            add_lesson(conn, "new-lesson-full-id");
            assert_eq!(
                delete_lesson_with_tombstone(conn, "old-lesson", Some("missing-id"), "")?,
                TombstoneDelete::InvalidSuccessor
            );
            assert_eq!(
                delete_lesson_with_tombstone(conn, "old-lesson", Some("old-lesson"), "")?,
                TombstoneDelete::InvalidSuccessor
            );
            assert!(live_lesson(conn, "old-lesson")?.is_some());
            assert_eq!(
                delete_lesson_with_tombstone(conn, "old-lesson", Some("new-less"), "retitled")?,
                TombstoneDelete::Deleted {
                    successor_id: Some("new-lesson-full-id".to_string())
                }
            );
            let t = get_tombstone(conn, "old-lesson")?.unwrap();
            assert_eq!(t.successor_id.as_deref(), Some("new-lesson-full-id"));
            assert_eq!(t.reason, "retitled");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn import_is_idempotent_and_reports() {
        let db = setup_db();
        db.with_conn(|conn| {
            add_lesson(conn, "still-live-lesson");
            add_lesson(conn, "successor-lesson-full");
            let tsv = "old_id\tnew_id\twhy\n\
                       retitled-old-1\tsuccessor-lesson-full\tretitled\n\
                       folded-old-0002\tsuccesso\tfolded by prefix\n\
                       deleted-old-03\t\tobsolete\n\
                       still-live-lesson\tsuccessor-lesson-full\tnot really gone\n\
                       still-li\t\tprefix of a live id\n\
                       lost-successor\tno-such-lesson\tpoints nowhere\n\
                       short\t\ttoo short\n";
            let first = import_tombstones(conn, tsv)?;
            assert_eq!(first.imported, 4);
            assert_eq!(first.updated, 0);
            assert_eq!(first.skipped_live, vec!["still-live-lesson", "still-li"]);
            assert_eq!(first.unresolved_successor, vec!["lost-successor"]);
            assert_eq!(first.invalid.len(), 1);
            assert_eq!(
                get_tombstone(conn, "folded-old-0002")?
                    .unwrap()
                    .successor_id,
                Some("successor-lesson-full".to_string())
            );

            let second = import_tombstones(conn, tsv)?;
            assert_eq!(second.imported, 0);
            assert_eq!(second.updated, 0);
            assert_eq!(second.unchanged, 4);
            assert_eq!(list_tombstones(conn)?.len(), 4);

            let changed =
                import_tombstones(conn, "old_id\tnew_id\twhy\ndeleted-old-03\t\tnew reason\n")?;
            assert_eq!(changed.updated, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn import_rejects_missing_header() {
        let db = setup_db();
        db.with_conn(|conn| {
            assert!(import_tombstones(conn, "abcdefgh\t\t\n").is_err());
            Ok(())
        })
        .unwrap();
    }
}
