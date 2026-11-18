//! Mention guard: the one server-side check that keeps an at-sign file
//! mention out of stored lesson and checkpoint text.
//!
//! Claude Code expands an at-sign mention in a prompt into a file read.
//! Lesson and checkpoint text is quoted into prompts (FLDSMDFR, bots pasting
//! a lesson into a message), so text in which an at-sign introduces a path
//! is **refused** before anything is stored, and text in which an at-sign
//! introduces a bare name is stored but **flagged** with a warning.
//!
//! The guard sits below every caller: [`mentions`] is called from
//! `storage::insert_lesson`, `storage::update_lesson` and
//! `storage::insert_checkpoint`, so REST, both MCP transports and every
//! in-process writer go through the same check. Clients keep no copy.
//!
//! What it matches (design v1c, 2026-11-17; fix round 1, 2026-11-18):
//!
//! * refuse: an at-sign not preceded by a word char or dot, followed by
//!   `~`, `/` or `.`, by a token containing `/` or `\` (other than a scoped
//!   package name, below), or by a file-name-shaped token (`name.ext`,
//!   extension a letter then 0 to 7 alphanumerics);
//! * flag: the same at-sign followed by a bare name (`[A-Za-z0-9_-]+`) with
//!   no dot or slash, or by a scoped package name `scope/name`
//!   (`[a-z0-9][a-z0-9_-]*/[a-z0-9][a-z0-9_-]*`, no dot, no `..`, no
//!   backslash, no second slash);
//! * never: an at-sign preceded by a word char or dot, which covers email
//!   addresses and the scp form `user@host:/path`.
//!
//! The reference pattern is `(?<![\w.])@(?:[~/.]|[^\s@]*[/\\]|[^\s@]+\.[A-Za-z][A-Za-z0-9]{0,7}\b)`,
//! with the scoped package shape carved out of the slash alternative.
//! The `regex` crate is not a dependency and has no look-behind, so the
//! scanner is written by hand; the "not preceded by `[\w.]`" test is in
//! [`scan`].

use serde::Serialize;

use crate::storage::{CheckpointRecord, LessonRecord};

/// The fix a refused caller is told to apply.
pub const FIX: &str = "write 'at-sign /path' in words";

/// Error code carried by a refusal body.
pub const ERROR_CODE: &str = "mention_refused";

/// Warning kind carried by a flagged bare name.
pub const BARE_AT_NAME: &str = "bare_at_name";

/// Warning kind carried by a flagged scoped package name (`scope/name`).
pub const SCOPED_AT_NAME: &str = "scoped_at_name";

/// What an at-sign introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A path or file name: refused.
    Path,
    /// A bare name with no dot or slash: flagged.
    BareName,
    /// A scoped package name, `scope/name` with no dot (one token-final dot
    /// allowed): flagged.
    ScopedName,
}

/// One at-sign hit in a text: the byte offset of the at-sign and its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Byte offset (UTF-8) of the at-sign in the scanned text.
    pub offset: usize,
    /// What the at-sign introduces.
    pub class: Class,
}

/// A refused write: which field, where, and the fix. The matched text is
/// never carried, so a client log does not re-quote the mention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("mention refused: field {field}, byte offset {offset}: {FIX}")]
pub struct Refusal {
    /// Field name, e.g. `content`, `tags[2]`, `state.done[1]`.
    pub field: String,
    /// Byte offset (UTF-8) of the at-sign in that field.
    pub offset: usize,
}

impl Refusal {
    /// The response body: `{"error","field","offset","fix"}`.
    #[must_use]
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "error": ERROR_CODE,
            "field": self.field,
            "offset": self.offset,
            "fix": FIX,
        })
    }
}

/// A flagged bare at-name: the write is stored, the caller is told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    /// Field name, as in [`Refusal`].
    pub field: String,
    /// Byte offset (UTF-8) of the at-sign in that field.
    pub offset: usize,
    /// `bare_at_name` or `scoped_at_name`.
    pub kind: &'static str,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `name.ext`: a dot at byte index 1 or later, then an ASCII letter and 0 to
/// 7 more ASCII alphanumerics, then a word boundary (end, or a non-word
/// char). An extension that starts with a digit (`93.2%`, `1.2.3`, `v1.2`)
/// is a number, not a file name.
fn is_file_name(token: &str) -> bool {
    let bytes = token.as_bytes();
    for (dot, _) in token.match_indices('.') {
        if dot == 0 || !bytes.get(dot + 1).is_some_and(u8::is_ascii_alphabetic) {
            continue;
        }
        let ext_len = bytes[dot + 1..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        if ext_len > 8 {
            continue;
        }
        let after = token[dot + 1 + ext_len..].chars().next();
        if !after.is_some_and(is_word) {
            return true;
        }
    }
    false
}

/// Length of the longest `[a-z0-9][a-z0-9_-]*` prefix of `s` (0 if none).
fn segment_len(s: &str) -> usize {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit() => {}
        _ => return 0,
    }
    1 + bytes
        .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
        .count()
}

/// `scope/name`, a scoped package name: no dot and no backslash anywhere in
/// the token, exactly one slash, the scope a whole `[a-z0-9][a-z0-9_-]*`
/// segment, the name one such segment followed by the end of the token or a
/// non-word char. Exactly one token-final dot (`scope/name.` ending a
/// sentence) is dropped first; the rest must pass unchanged (Bilby,
/// 2026-11-18), so `scope/name..` and `scope/name.x.` stay paths.
fn is_scoped_name(token: &str) -> bool {
    let token = token.strip_suffix('.').unwrap_or(token);
    if token.contains(['.', '\\']) {
        return false;
    }
    let Some((scope, rest)) = token.split_once('/') else {
        return false;
    };
    if rest.contains('/') || scope.is_empty() || segment_len(scope) != scope.len() {
        return false;
    }
    let name_len = segment_len(rest);
    name_len > 0 && !rest[name_len..].chars().next().is_some_and(is_word)
}

/// Classify the token that follows an at-sign.
fn classify(token: &str) -> Option<Class> {
    let first = token.chars().next()?;
    if matches!(first, '~' | '/' | '.') {
        return Some(Class::Path);
    }
    if is_scoped_name(token) {
        return Some(Class::ScopedName);
    }
    if token.contains(['/', '\\']) || is_file_name(token) {
        return Some(Class::Path);
    }
    if first.is_ascii_alphanumeric() || first == '_' || first == '-' {
        return Some(Class::BareName);
    }
    None
}

/// Scan one text for at-sign mentions. Every hit is returned in order.
///
/// An at-sign preceded by a word char or a dot is skipped: that is the
/// look-behind `(?<![\w.])` of the reference pattern, done in code.
#[must_use]
pub fn scan(text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut prev: Option<char> = None;
    for (offset, c) in text.char_indices() {
        if c == '@' && !prev.is_some_and(|p| is_word(p) || p == '.') {
            let rest = &text[offset + 1..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '@')
                .unwrap_or(rest.len());
            if let Some(class) = classify(&rest[..end]) {
                hits.push(Hit { offset, class });
            }
        }
        prev = Some(c);
    }
    hits
}

/// The guard. Scans `(field name, text)` pairs in order; the first path
/// mention refuses the whole write, otherwise every bare name is a warning.
///
/// # Errors
///
/// Returns a [`Refusal`] naming the first field and offset in which an
/// at-sign introduces a path.
pub fn mentions<'a, I>(fields: I) -> Result<Vec<Warning>, Refusal>
where
    I: IntoIterator<Item = (String, &'a str)>,
{
    let mut warnings = Vec::new();
    for (field, text) in fields {
        for hit in scan(text) {
            match hit.class {
                Class::Path => {
                    return Err(Refusal {
                        field,
                        offset: hit.offset,
                    })
                }
                Class::BareName => warnings.push(Warning {
                    field: field.clone(),
                    offset: hit.offset,
                    kind: BARE_AT_NAME,
                }),
                Class::ScopedName => warnings.push(Warning {
                    field: field.clone(),
                    offset: hit.offset,
                    kind: SCOPED_AT_NAME,
                }),
            }
        }
    }
    Ok(warnings)
}

/// Every string anywhere inside a JSON value.
///
/// Walked recursively through objects and arrays, named `prefix`,
/// `prefix.key`, `prefix[i]`. Object keys are scanned too; a key that itself
/// contains an at-sign is named by ordinal (`prefix.key[n]`) so the name
/// does not echo it.
pub fn json_fields<'a>(
    prefix: &str,
    value: &'a serde_json::Value,
    out: &mut Vec<(String, &'a str)>,
) {
    match value {
        serde_json::Value::String(s) => out.push((prefix.to_string(), s.as_str())),
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                json_fields(&format!("{prefix}[{i}]"), item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (n, (key, item)) in map.iter().enumerate() {
                let shown = if key.contains('@') {
                    format!("key[{n}]")
                } else {
                    key.clone()
                };
                out.push((format!("{prefix}.key[{n}]"), key.as_str()));
                json_fields(&format!("{prefix}.{shown}"), item, out);
            }
        }
        _ => {}
    }
}

/// The lesson fields the guard reads: title, content, every tag, agent, repo.
#[must_use]
pub fn lesson_fields(lesson: &LessonRecord) -> Vec<(String, &str)> {
    let mut fields = vec![
        ("title".to_string(), lesson.title.as_str()),
        ("content".to_string(), lesson.content.as_str()),
    ];
    fields.extend(
        lesson
            .tags
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("tags[{i}]"), t.as_str())),
    );
    if let Some(agent) = &lesson.agent {
        fields.push(("agent".to_string(), agent.as_str()));
    }
    if let Some(repo) = &lesson.repo {
        fields.push(("repo".to_string(), repo.as_str()));
    }
    fields
}

/// The checkpoint fields the guard reads: agent, working_on, repo,
/// session_id, and every string inside `state`.
#[must_use]
pub fn checkpoint_fields(checkpoint: &CheckpointRecord) -> Vec<(String, &str)> {
    let mut fields = vec![
        ("agent".to_string(), checkpoint.agent.as_str()),
        ("working_on".to_string(), checkpoint.working_on.as_str()),
    ];
    if let Some(repo) = &checkpoint.repo {
        fields.push(("repo".to_string(), repo.as_str()));
    }
    if let Some(session_id) = &checkpoint.session_id {
        fields.push(("session_id".to_string(), session_id.as_str()));
    }
    json_fields("state", &checkpoint.state, &mut fields);
    fields
}

/// Guard a lesson record.
///
/// # Errors
///
/// See [`mentions`].
pub fn lesson(lesson: &LessonRecord) -> Result<Vec<Warning>, Refusal> {
    mentions(lesson_fields(lesson))
}

/// Guard a checkpoint record.
///
/// # Errors
///
/// See [`mentions`].
pub fn checkpoint(checkpoint: &CheckpointRecord) -> Result<Vec<Warning>, Refusal> {
    mentions(checkpoint_fields(checkpoint))
}

/// Guard request fields that are not part of the stored record.
///
/// These are the MCP graph fields (`solved_problem`, `used_tools`,
/// `related_concepts`, `learned_from`, `tools_used`, `problems_encountered`,
/// `solutions_found`), which become graph entity names. Only the named keys
/// are read.
///
/// # Errors
///
/// See [`mentions`].
pub fn json_keys(args: &serde_json::Value, keys: &[&str]) -> Result<Vec<Warning>, Refusal> {
    let mut fields = Vec::new();
    for key in keys {
        if let Some(value) = args.get(key) {
            json_fields(key, value, &mut fields);
        }
    }
    mentions(fields)
}

/// Counts from a read-only scan of stored rows: how many lessons and
/// checkpoints carry each class. A row is counted once, in its worst class.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ScanCounts {
    pub lessons_total: u64,
    pub lessons_refuse: u64,
    pub lessons_flag: u64,
    pub checkpoints_total: u64,
    pub checkpoints_refuse: u64,
    pub checkpoints_flag: u64,
}

fn count_row<'a, I>(fields: I, refuse: &mut u64, flag: &mut u64)
where
    I: IntoIterator<Item = (String, &'a str)>,
{
    match mentions(fields) {
        Err(_) => *refuse += 1,
        Ok(w) if !w.is_empty() => *flag += 1,
        Ok(_) => {}
    }
}

/// Read-only, counts-only scan of every stored lesson and checkpoint.
/// Reads the rows and nothing else; never rewrites.
///
/// # Errors
///
/// Returns the database error if a SELECT fails.
pub fn scan_counts(conn: &rusqlite::Connection) -> rusqlite::Result<ScanCounts> {
    let mut counts = ScanCounts::default();

    let mut stmt = conn.prepare("SELECT title, content, tags, agent, repo FROM lessons")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (title, content, tags_json, agent, repo) = row?;
        counts.lessons_total += 1;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        let mut record = LessonRecord::new(title, content, tags);
        record.agent = agent;
        record.repo = repo;
        count_row(
            lesson_fields(&record),
            &mut counts.lessons_refuse,
            &mut counts.lessons_flag,
        );
    }

    let mut stmt =
        conn.prepare("SELECT agent, working_on, state, repo, session_id FROM checkpoints")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (agent, working_on, state_json, repo, session_id) = row?;
        counts.checkpoints_total += 1;
        let state: serde_json::Value = serde_json::from_str(&state_json).unwrap_or_default();
        let mut record = CheckpointRecord::new(agent, working_on, state);
        record.repo = repo;
        record.session_id = session_id;
        count_row(
            checkpoint_fields(&record),
            &mut counts.checkpoints_refuse,
            &mut counts.checkpoints_flag,
        );
    }

    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_of(text: &str) -> Option<Class> {
        let hits = scan(text);
        assert!(hits.len() <= 1, "{text:?}: {hits:?}");
        hits.first().map(|h| h.class)
    }

    // Fixtures only: an at-sign before a path is the thing under test.
    const REFUSE: &[&str] = &[
        "@/etc/x",
        "@~/.ssh/x",
        "@./x",
        "@../x",
        "@docs/a.md",
        "@README.md",
        "@.bashrc",
        "see @/etc/x for details",
        "read it\n@/etc/x",
        "(@/etc/x)",
        "`@/etc/x`",
        "curl -d @/etc/passwd",
        "@a\\b",
        "@Makefile.in,",
    ];
    // A bare name followed by a dot that starts no 1-8 char extension is still a
    // bare name: the flag pattern `@[A-Za-z0-9_-]+` stops before the dot.
    const FLAG: &[&str] = &[
        "@skippy",
        "@mmorris35",
        "hi @skippy!",
        "@Makefile",
        "@a_b-c",
        "@foo.",
        "@foo.toolongext1",
    ];
    const CLEAN: &[&str] = &[
        "liam@example.com",
        "user@host:/path",
        "a@b",
        "write at-sign /path in words",
        "x.@/etc/x",
        "@ /etc/x",
        "@",
        "@@",
        "@(foo)",
        "",
    ];

    #[test]
    fn refuse_twins() {
        for t in REFUSE {
            assert_eq!(class_of(t), Some(Class::Path), "{t:?}");
        }
    }

    #[test]
    fn flag_twins() {
        for t in FLAG {
            assert_eq!(class_of(t), Some(Class::BareName), "{t:?}");
        }
    }

    #[test]
    fn clean_twins() {
        for t in CLEAN {
            assert_eq!(class_of(t), None, "{t:?}");
        }
    }

    /// Mutant 1: the look-behind check removed. The email and scp twins
    /// are the ones that fail.
    #[test]
    fn look_behind_email_and_scp_are_clean() {
        assert_eq!(scan("liam@example.com"), []);
        assert_eq!(scan("user@host:/path"), []);
        assert_eq!(scan("v1.@/x"), []);
    }

    /// Mutant 2: the relative (name.ext) alternative removed. This twin fails.
    #[test]
    fn relative_file_name_is_refused() {
        assert_eq!(class_of("@README.md"), Some(Class::Path));
        assert_eq!(class_of("see @notes.txt."), Some(Class::Path));
    }

    /// Fix round 1 (Bilby, 2026-11-18): a scoped package name, `scope/name`
    /// with no dot, no `..`, no backslash and no second slash, is a flag.
    const SCOPED: &[&str] = &[
        "@anthropic-ai/sdk",
        "@anthropic-ai/claude-agent-sdk",
        "@modelcontextprotocol/sdk",
        "@types/node",
        "use @types/node, then",
        "(@types/node)",
    ];

    #[test]
    fn scoped_package_names_are_flagged() {
        for t in SCOPED {
            assert_eq!(class_of(t), Some(Class::ScopedName), "{t:?}");
        }
    }

    /// Mutant 4: the dot exclusion dropped from the scoped rule. The run.sh
    /// twin (a dotted name segment) is the one that fails.
    #[test]
    fn dotted_scoped_name_is_refused() {
        assert_eq!(class_of("@scripts/run.sh"), Some(Class::Path));
        assert_eq!(class_of("@config/x.env"), Some(Class::Path));
        assert_eq!(class_of("@types/node.js"), Some(Class::Path));
    }

    #[test]
    fn scoped_shapes_with_dotdot_or_second_slash_are_refused() {
        for t in [
            "@a/../b",
            "@a/b/c",
            "@a/b\\c",
            "@a/",
            "@/a/b",
            "@Types/node",
            "@types/Node",
            "@a_b-c/d_e-f/",
        ] {
            assert_eq!(class_of(t), Some(Class::Path), "{t:?}");
        }
    }

    #[test]
    fn scoped_package_is_stored_with_a_scoped_warning() {
        use crate::storage::{get_lesson, insert_lesson, migrate, Database};
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            let lesson = LessonRecord::new("pkg", "pin @types/node", vec![]);
            let w = insert_lesson(conn, &lesson)?;
            assert_eq!(w.len(), 1);
            assert_eq!(w[0].field, "content");
            assert_eq!(w[0].offset, 4);
            assert_eq!(w[0].kind, SCOPED_AT_NAME);
            assert_eq!(get_lesson(conn, &lesson.id)?.content, "pin @types/node");
            assert!(insert_lesson(
                conn,
                &LessonRecord::new("pkg", "run @scripts/run.sh", vec![])
            )
            .is_err());
            Ok(())
        })
        .unwrap();
    }

    /// Fix round 2 (Bilby ruling 1, 2026-11-18): a token whose last char is
    /// exactly one `.` after a clean `scope/name` is that scoped name at a
    /// sentence end: flagged, kind `scoped_at_name`.
    #[test]
    fn scoped_name_with_one_final_dot_is_flagged() {
        for t in [
            "@types/node.",
            "pin @types/node.",
            "@anthropic-ai/sdk.",
            "use @modelcontextprotocol/sdk.",
        ] {
            assert_eq!(class_of(t), Some(Class::ScopedName), "{t:?}");
        }
    }

    /// Mutant 6: every trailing dot stripped instead of exactly one. The `..`
    /// twin is the one that fails; the rest stay refused under both.
    #[test]
    fn scoped_name_with_two_final_dots_or_an_extension_is_refused() {
        for t in [
            "@types/node..",
            "@types/node.js",
            "@types/node.x.",
            "run @scripts/run.sh.",
        ] {
            assert_eq!(class_of(t), Some(Class::Path), "{t:?}");
        }
    }

    #[test]
    fn scoped_name_with_one_final_dot_is_stored_with_a_scoped_warning() {
        use crate::storage::{insert_lesson, migrate, Database};
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            let w = insert_lesson(conn, &LessonRecord::new("pkg", "pin @types/node.", vec![]))?;
            assert_eq!(w.len(), 1);
            assert_eq!(w[0].kind, SCOPED_AT_NAME);
            assert!(
                insert_lesson(conn, &LessonRecord::new("pkg", "pin @types/node..", vec![]))
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
    }

    /// Mutant 5: the leading-letter rule dropped from `name.ext`. The numeric
    /// twins are the ones that fail: under the unchanged flag rule they are
    /// bare names (`93`, `1`, `v1`, stopping before the dot), never a path.
    #[test]
    fn numeric_extension_is_not_a_file_name() {
        for t in ["@93.2%", "@1.2.3", "@v1.2", "hit @93.2% today", "@a.1b"] {
            assert_eq!(class_of(t), Some(Class::BareName), "{t:?}");
        }
        assert_eq!(class_of("@README.md"), Some(Class::Path));
        assert_eq!(class_of("@notes.txt"), Some(Class::Path));
        assert_eq!(class_of("@a.b1"), Some(Class::Path));
    }

    #[test]
    fn offsets_are_byte_offsets_of_the_at_sign() {
        let hits = scan("héllo @/x and @bob");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].offset, "héllo ".len());
        assert_eq!(hits[0].class, Class::Path);
        assert_eq!(hits[1].offset, "héllo @/x and ".len());
        assert_eq!(hits[1].class, Class::BareName);
    }

    #[test]
    fn mentions_refuses_on_first_path_and_names_field() {
        let fields = vec![
            ("title".to_string(), "fine @bob"),
            ("content".to_string(), "x\n@/etc/x"),
        ];
        let r = mentions(fields).unwrap_err();
        assert_eq!(
            r,
            Refusal {
                field: "content".to_string(),
                offset: 2
            }
        );
        let body = r.body();
        assert_eq!(body["error"], ERROR_CODE);
        assert_eq!(body["field"], "content");
        assert_eq!(body["offset"], 2);
        assert_eq!(body["fix"], FIX);
        assert!(!body.to_string().contains("/etc"));
        assert!(!r.to_string().contains("/etc"));
    }

    #[test]
    fn mentions_collects_warnings() {
        let fields = vec![
            ("title".to_string(), "@bob and @amy"),
            ("tags[0]".to_string(), "@tag"),
        ];
        let w = mentions(fields).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].field, "title");
        assert_eq!(w[0].offset, 0);
        assert_eq!(w[0].kind, BARE_AT_NAME);
        assert_eq!(w[2].field, "tags[0]");
    }

    #[test]
    fn lesson_fields_cover_title_content_tags_agent_repo() {
        let mut l = LessonRecord::new("t", "c", vec!["a".into(), "b".into()]);
        l.agent = Some("ag".into());
        l.repo = Some("rp".into());
        let names: Vec<String> = lesson_fields(&l).into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            ["title", "content", "tags[0]", "tags[1]", "agent", "repo"]
        );
    }

    /// Mutant 3: the checkpoint walk limited to top-level fields. The nested
    /// state.done twin (an array inside an object) is the one that fails.
    #[test]
    fn checkpoint_nested_state_done_is_refused() {
        let cp = CheckpointRecord::new(
            "kahuna",
            "clean",
            serde_json::json!({"done": ["ok", "then @/etc/x"], "n": 1}),
        );
        let r = checkpoint(&cp).unwrap_err();
        assert_eq!(r.field, "state.done[1]");
        assert_eq!(r.offset, 5);
    }

    #[test]
    fn checkpoint_walk_reaches_objects_in_arrays_and_keys() {
        let cp = CheckpointRecord::new(
            "kahuna",
            "x",
            serde_json::json!({"steps": [{"note": "@~/.ssh/x"}]}),
        );
        assert_eq!(checkpoint(&cp).unwrap_err().field, "state.steps[0].note");

        let cp = CheckpointRecord::new("kahuna", "x", serde_json::json!({"@/etc/x": "v"}));
        let r = checkpoint(&cp).unwrap_err();
        assert_eq!(r.field, "state.key[0]");
        assert!(!r.field.contains("/etc"));

        let cp = CheckpointRecord::new("kahuna", "@/etc/x", serde_json::json!({}));
        assert_eq!(checkpoint(&cp).unwrap_err().field, "working_on");

        let cp = CheckpointRecord::new(
            "kahuna",
            "ping @skippy",
            serde_json::json!({"a": [1, null, true]}),
        );
        let w = checkpoint(&cp).unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].field, "working_on");
    }

    #[test]
    fn json_keys_reads_only_the_named_keys() {
        let args = serde_json::json!({
            "content": "@/etc/x",
            "solved_problem": "ok @bob",
            "used_tools": ["@/x"],
        });
        let r = json_keys(&args, &["solved_problem", "used_tools"]).unwrap_err();
        assert_eq!(r.field, "used_tools[0]");
        let w = json_keys(&args, &["solved_problem"]).unwrap();
        assert_eq!(w.len(), 1);
        assert_eq!(json_keys(&args, &["learned_from"]).unwrap(), []);
    }

    #[test]
    fn scan_counts_classes_each_row_once() {
        use crate::storage::{insert_checkpoint, insert_lesson, migrate, Database};
        let db = Database::open_in_memory().unwrap();
        db.with_conn(|conn| {
            migrate(conn)?;
            // Rows written before the guard existed: raw SQL, as the live DB has them.
            conn.execute(
                "INSERT INTO lessons (id, title, content, tags, severity, created_at, updated_at)
                 VALUES ('l1', 'old', 'see @/etc/x and @bob', '[]', 'info', 1, 1)",
                [],
            )
            .map_err(|e| crate::error::StorageError::Database(e.to_string()))?;
            conn.execute(
                "INSERT INTO lessons (id, title, content, tags, severity, created_at, updated_at)
                 VALUES ('l2', 'old', 'ping @bob', '[\"@amy\"]', 'info', 1, 1)",
                [],
            )
            .map_err(|e| crate::error::StorageError::Database(e.to_string()))?;
            insert_lesson(conn, &LessonRecord::new("clean", "clean", vec![]))?;
            conn.execute(
                "INSERT INTO checkpoints (id, agent, working_on, state, created_at)
                 VALUES ('c1', 'a', 'w', '{\"done\":[\"@/x\"]}', 1)",
                [],
            )
            .map_err(|e| crate::error::StorageError::Database(e.to_string()))?;
            insert_checkpoint(
                conn,
                &CheckpointRecord::new("a", "w", serde_json::json!({})),
            )?;
            let counts = scan_counts(conn)
                .map_err(|e| crate::error::StorageError::Database(e.to_string()))?;
            assert_eq!(
                counts,
                ScanCounts {
                    lessons_total: 3,
                    lessons_refuse: 1,
                    lessons_flag: 1,
                    checkpoints_total: 2,
                    checkpoints_refuse: 1,
                    checkpoints_flag: 0,
                }
            );
            Ok(())
        })
        .unwrap();
    }
}
