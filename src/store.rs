//! Workspace persistence — the Canon Cat branch's database-backed store.
//!
//! There are no files on this branch. The entire workspace is one continuous
//! text stream persisted in SQLite as an **append-only edit log**: every insert
//! and delete is a row, and the text is reconstructed by replaying the log on
//! startup. Because nothing is ever overwritten, nothing is ever lost — power
//! off mid-keystroke and you resume exactly where you were (the Cat's promise).
//!
//! See `docs/CANON_CAT.md` for the data model. This module is storage only: it
//! knows about [`EditOp`]s, snapshots, and resume state, but nothing about the
//! rope, the cursor semantics, or the terminal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// Current on-disk schema version, bumped when the table layout changes.
const SCHEMA_VERSION: i64 = 1;

/// One edit applied to the text stream, as recorded in the append-only log.
///
/// Positions are **char indices** into the stream (matching the rope's cursor
/// model). A delete carries the text it removed so undo can restore it without
/// consulting any other state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOp {
    /// `text` was inserted, its first char landing at char index `pos`.
    Insert { pos: usize, text: String },
    /// `text` was removed; it had started at char index `pos`.
    Delete { pos: usize, text: String },
}

impl EditOp {
    /// Apply this edit to a plain `String` (used when replaying the log).
    fn apply(&self, s: &mut String) {
        match self {
            EditOp::Insert { pos, text } => {
                let at = char_to_byte(s, *pos);
                s.insert_str(at, text);
            }
            EditOp::Delete { pos, text } => {
                let start = char_to_byte(s, *pos);
                let end = char_to_byte(s, pos + text.chars().count());
                s.replace_range(start..end, "");
            }
        }
    }

    fn op_code(&self) -> i64 {
        match self {
            EditOp::Insert { .. } => 0,
            EditOp::Delete { .. } => 1,
        }
    }

    fn pos(&self) -> usize {
        match self {
            EditOp::Insert { pos, .. } | EditOp::Delete { pos, .. } => *pos,
        }
    }

    fn body(&self) -> &str {
        match self {
            EditOp::Insert { text, .. } | EditOp::Delete { text, .. } => text,
        }
    }
}

/// Everything needed to put the editor back exactly where it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resume {
    /// The reconstructed text stream.
    pub text: String,
    /// Cursor as a char index into `text`.
    pub cursor: usize,
    /// First visible line (vertical scroll).
    pub top: usize,
    /// First visible display column (horizontal scroll).
    pub left: usize,
}

/// The workspace database. Owns a single SQLite connection.
pub struct Store {
    conn: Connection,
    /// The live history tip (last applied edit's `seq`); `0` means empty.
    head: i64,
}

impl Store {
    /// Path to the single fixed workspace database under the XDG data dir.
    pub fn default_path() -> Result<PathBuf> {
        let dir = dirs::data_dir().context("no XDG data dir (set $XDG_DATA_HOME or $HOME)")?;
        Ok(dir.join("leap").join("workspace.db"))
    }

    /// Open (creating if needed) the workspace at `path`, initialising the
    /// schema. Parent directories are created. WAL mode keeps per-keystroke
    /// commits cheap and crash-safe.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening workspace {}", path.display()))?;
        Self::from_conn(conn)
    }

    /// An ephemeral in-memory workspace (for tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS snapshot(upto_seq INTEGER NOT NULL, text TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS edits(
                 seq      INTEGER PRIMARY KEY AUTOINCREMENT,
                 parent   INTEGER NOT NULL,
                 op       INTEGER NOT NULL,
                 pos      INTEGER NOT NULL,
                 body     TEXT NOT NULL,
                 group_id INTEGER NOT NULL,
                 ts       INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS state(
                 id     INTEGER PRIMARY KEY CHECK(id = 1),
                 head   INTEGER NOT NULL,
                 cursor INTEGER NOT NULL,
                 top    INTEGER NOT NULL,
                 left   INTEGER NOT NULL);",
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO state(id, head, cursor, top, left) VALUES (1, 0, 0, 0, 0)",
            [],
        )?;
        let head: i64 = conn.query_row("SELECT head FROM state WHERE id = 1", [], |r| r.get(0))?;
        Ok(Self { conn, head })
    }

    /// The live history tip (`seq` of the last applied edit, or 0 if empty).
    pub fn head(&self) -> i64 {
        self.head
    }

    /// Append a batch of edits (one keystroke may produce several) as a single
    /// transaction, advancing the live head. `group_id` ties a coalescing run
    /// together for undo. Returns the new head `seq`.
    pub fn append(&mut self, ops: &[EditOp], group_id: i64) -> Result<i64> {
        if ops.is_empty() {
            return Ok(self.head);
        }
        let ts = now_millis();
        let tx = self.conn.transaction()?;
        let mut head = self.head;
        for op in ops {
            tx.execute(
                "INSERT INTO edits(parent, op, pos, body, group_id, ts)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![head, op.op_code(), op.pos() as i64, op.body(), group_id, ts],
            )?;
            head = tx.last_insert_rowid();
        }
        tx.execute("UPDATE state SET head = ?1 WHERE id = 1", params![head])?;
        tx.commit()?;
        self.head = head;
        Ok(head)
    }

    /// Persist the resume cursor/scroll. Cheap single-row update; called as the
    /// view moves.
    pub fn set_state(&mut self, cursor: usize, top: usize, left: usize) -> Result<()> {
        self.conn.execute(
            "UPDATE state SET cursor = ?1, top = ?2, left = ?3 WHERE id = 1",
            params![cursor as i64, top as i64, left as i64],
        )?;
        Ok(())
    }

    /// Reconstruct the full workspace: snapshot text + replayed edits up to the
    /// live head, plus the saved cursor/scroll.
    pub fn resume(&self) -> Result<Resume> {
        let (upto, mut text): (i64, String) = self
            .conn
            .query_row("SELECT upto_seq, text FROM snapshot LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .unwrap_or((0, String::new()));

        for op in self.chain(upto, self.head)? {
            op.apply(&mut text);
        }

        let (cursor, top, left): (i64, i64, i64) = self.conn.query_row(
            "SELECT cursor, top, left FROM state WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;

        Ok(Resume {
            text,
            cursor: cursor as usize,
            top: top as usize,
            left: left as usize,
        })
    }

    /// All edit rows keyed by `seq`. The workspace is bounded and the log is
    /// compacted, so this stays small; undo/redo and replay walk it in memory.
    fn load_edits(&self) -> Result<HashMap<i64, Edit>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, parent, op, pos, body, group_id FROM edits")?;
        let rows = stmt.query_map([], |r| {
            Ok(Edit {
                seq: r.get(0)?,
                parent: r.get(1)?,
                group: r.get(5)?,
                op: decode_op(r.get(2)?, r.get::<_, i64>(3)? as usize, r.get(4)?),
            })
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let e = row?;
            map.insert(e.seq, e);
        }
        Ok(map)
    }

    /// The edits on the live history chain from `upto` (exclusive) to `head`, in
    /// application order — following `parent` pointers, so orphaned (undone-then-
    /// superseded) branches are skipped.
    fn chain(&self, upto: i64, head: i64) -> Result<Vec<EditOp>> {
        let map = self.load_edits()?;
        let mut chain = Vec::new();
        let mut cur = head;
        while cur > upto && cur != 0 {
            let Some(e) = map.get(&cur) else { break };
            chain.push(e.op.clone());
            cur = e.parent;
        }
        chain.reverse();
        Ok(chain)
    }

    fn set_head(&mut self, head: i64) -> Result<()> {
        self.conn
            .execute("UPDATE state SET head = ?1 WHERE id = 1", params![head])?;
        self.head = head;
        Ok(())
    }

    /// Undo the most recent edit group: move the head back past the group and
    /// return the **inverse** ops (newest first) for the caller to apply to the
    /// buffer. Empty when there's nothing to undo. Rows are never deleted, so a
    /// later redo (or a new edit, which forks) can still reach them.
    pub fn undo(&mut self) -> Result<Vec<EditOp>> {
        if self.head == 0 {
            return Ok(Vec::new());
        }
        let map = self.load_edits()?;
        let Some(top) = map.get(&self.head) else {
            return Ok(Vec::new());
        };
        let group = top.group;
        let mut inverses = Vec::new();
        let mut cur = self.head;
        let mut new_head = 0;
        while let Some(e) = map.get(&cur) {
            if e.group != group {
                break;
            }
            inverses.push(invert(&e.op));
            new_head = e.parent;
            cur = e.parent;
            if cur == 0 {
                break;
            }
        }
        self.set_head(new_head)?;
        Ok(inverses)
    }

    /// Redo the next group — the most-recently-created child branch of the head.
    /// Returns the forward ops (application order) and advances the head. Empty
    /// when there's nothing to redo.
    pub fn redo(&mut self) -> Result<Vec<EditOp>> {
        let map = self.load_edits()?;
        let Some(start) = map
            .values()
            .filter(|e| e.parent == self.head)
            .map(|e| e.seq)
            .max()
        else {
            return Ok(Vec::new());
        };
        let group = map[&start].group;
        let mut ops = Vec::new();
        let mut cur = start;
        loop {
            let e = &map[&cur];
            if e.group != group {
                break;
            }
            ops.push(e.op.clone());
            match map
                .values()
                .filter(|c| c.parent == cur && c.group == group)
                .map(|c| c.seq)
                .max()
            {
                Some(next) => cur = next,
                None => break,
            }
        }
        self.set_head(cur)?;
        Ok(ops)
    }

    /// Compact the log: record `text` as the snapshot at the current head and
    /// drop edits it now subsumes. Call on clean quit or past a size threshold.
    pub fn snapshot(&mut self, text: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM snapshot", [])?;
        tx.execute(
            "INSERT INTO snapshot(upto_seq, text) VALUES (?1, ?2)",
            params![self.head, text],
        )?;
        tx.execute("DELETE FROM edits WHERE seq <= ?1", params![self.head])?;
        tx.commit()?;
        Ok(())
    }

    /// Number of edit-log rows currently stored (drives the compaction trigger).
    pub fn edit_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM edits", [], |r| r.get(0))?)
    }
}

/// An edit-log row loaded for chain walking / undo / redo.
struct Edit {
    seq: i64,
    parent: i64,
    group: i64,
    op: EditOp,
}

/// Decode an `(op_code, pos, body)` row into an [`EditOp`].
fn decode_op(op: i64, pos: usize, text: String) -> EditOp {
    match op {
        0 => EditOp::Insert { pos, text },
        _ => EditOp::Delete { pos, text },
    }
}

/// The inverse of an edit (what reverses it when applied to the buffer).
fn invert(op: &EditOp) -> EditOp {
    match op {
        EditOp::Insert { pos, text } => EditOp::Delete { pos: *pos, text: text.clone() },
        EditOp::Delete { pos, text } => EditOp::Insert { pos: *pos, text: text.clone() },
    }
}

/// Byte offset of char index `i` in `s` (clamped to the end).
fn char_to_byte(s: &str, i: usize) -> usize {
    s.char_indices()
        .nth(i)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(pos: usize, text: &str) -> EditOp {
        EditOp::Insert { pos, text: text.into() }
    }
    fn del(pos: usize, text: &str) -> EditOp {
        EditOp::Delete { pos, text: text.into() }
    }

    #[test]
    fn empty_workspace_resumes_blank() {
        let s = Store::open_in_memory().unwrap();
        let r = s.resume().unwrap();
        assert_eq!(r, Resume::default());
    }

    #[test]
    fn append_then_resume_reconstructs_text() {
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "hello")], 1).unwrap();
        s.append(&[ins(5, " world")], 2).unwrap();
        assert_eq!(s.resume().unwrap().text, "hello world");
    }

    #[test]
    fn delete_replays_correctly() {
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "hello world")], 1).unwrap();
        // Remove "lo wor" (chars 3..9), leaving "helld".
        s.append(&[del(3, "lo wor")], 2).unwrap();
        assert_eq!(s.resume().unwrap().text, "helld");
    }

    #[test]
    fn batched_ops_in_one_append() {
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "ab"), del(0, "a"), ins(1, "Z")], 1).unwrap();
        assert_eq!(s.resume().unwrap().text, "bZ");
    }

    #[test]
    fn state_round_trips() {
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "abcdef")], 1).unwrap();
        s.set_state(4, 0, 2).unwrap();
        let r = s.resume().unwrap();
        assert_eq!((r.cursor, r.top, r.left), (4, 0, 2));
    }

    #[test]
    fn snapshot_compacts_but_preserves_text() {
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "one ")], 1).unwrap();
        s.append(&[ins(4, "two ")], 2).unwrap();
        let text = s.resume().unwrap().text;
        s.snapshot(&text).unwrap();
        assert_eq!(s.edit_count().unwrap(), 0, "log compacted");
        // Further edits stack on top of the snapshot.
        s.append(&[ins(8, "three")], 3).unwrap();
        assert_eq!(s.resume().unwrap().text, "one two three");
    }

    #[test]
    fn undo_redo_move_the_head_for_resume() {
        // resume() reconstructs from the head, so undo/redo survive a restart.
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "ab")], 1).unwrap();
        s.append(&[ins(2, "cd")], 1).unwrap(); // same group → one undo step
        assert_eq!(s.resume().unwrap().text, "abcd");

        assert!(!s.undo().unwrap().is_empty());
        assert_eq!(s.resume().unwrap().text, ""); // head walked back past the group

        assert!(!s.redo().unwrap().is_empty());
        assert_eq!(s.resume().unwrap().text, "abcd");

        assert!(s.redo().unwrap().is_empty()); // nothing left to redo
    }

    #[test]
    fn new_edit_after_undo_forks_and_orphans_redo() {
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "abc")], 1).unwrap();
        s.undo().unwrap(); // back to empty
        s.append(&[ins(0, "X")], 2).unwrap(); // forks; the "abc" branch is orphaned
        assert_eq!(s.resume().unwrap().text, "X");
        assert!(s.redo().unwrap().is_empty()); // can't redo across the new edit
    }

    #[test]
    fn unicode_positions_use_char_indices() {
        let mut s = Store::open_in_memory().unwrap();
        s.append(&[ins(0, "héllo")], 1).unwrap();
        // Insert at char index 2 (after 'é'), not byte index.
        s.append(&[ins(2, "X")], 2).unwrap();
        assert_eq!(s.resume().unwrap().text, "héXllo");
    }

    #[test]
    fn reopen_file_db_persists_across_instances() {
        let dir = std::env::temp_dir().join(format!("leap_store_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ws.db");
        {
            let mut s = Store::open(&path).unwrap();
            s.append(&[ins(0, "persist me")], 1).unwrap();
            s.set_state(3, 0, 0).unwrap();
        }
        {
            let s = Store::open(&path).unwrap();
            let r = s.resume().unwrap();
            assert_eq!(r.text, "persist me");
            assert_eq!(r.cursor, 3);
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
