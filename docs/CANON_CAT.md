# leap — Canon Cat branch

Branch: **`canon-cat`**. An experiment that takes `leap` much closer to Jef
Raskin's **Canon Cat**: no files, no save command, no open dialog. There is one
continuous text stream that *is* your workspace; the machine resumes exactly where
you left off; documents are spans of that stream separated by boundary markers.

This branch diverges from `main`'s file-based model. It is not meant to merge back
wholesale — it's a parallel exploration of the Cat interaction.

## What stays vs. what changes

Reused unchanged: `ropey` buffer core (cursor, movement, editing primitives),
LEAP (`src/leap.rs`, `C-s`/`C-r` incremental search-to-move), the diff renderer,
`src/hold.rs` (hold-to-confirm), `src/echo.rs` (display-only message line),
`src/statusline.rs`, the Kitty-protocol negotiation in `src/terminal.rs`.

Dropped on this branch: external files. `src/finder.rs` and `src/walk.rs` (fzf
file finder), the `ignore`/`nucleo-matcher` deps, `Buffer`'s file I/O
(`open`/`save`/`save_as`/`load`/EOL handling), and the `FILE`/`--line`/`--readonly`
CLI surface that pointed at files.

## Decisions (locked — interview 2026-06-08)

| Area | Decision |
|------|----------|
| Text model | **One continuous stream.** A single `ropey` rope holds everything. Document boundaries are **in-band marker characters** embedded in the text (a real char, so offsets are simple and LEAP roams the whole stream). |
| Persistence | **Append-only edit log in SQLite.** Every edit is a row (insert/delete + char position + the text). The rope is reconstructed by replaying the log on startup. Periodic full-text **snapshots** compact the log. |
| Save timing | **Every keystroke** (edits flushed per key, debounced only enough to batch a single key's ops into one transaction). Power off anytime; nothing is lost. No save command. |
| Files | **Pure DB.** No file open/save anywhere. The workspace *is* the database. |
| New document | A **dedicated chord** inserts a document-boundary marker at the cursor, splitting the stream. The marker renders as a full-width rule on its own line. |
| Document nav | **Jump keys + LEAP.** Next/prev-document keys jump boundary-to-boundary; ordinary LEAP still flies to a document by typing its leading text. |
| Undo | **Scrub the edit log.** Undo/redo walk the persisted log (with run coalescing); a parent pointer threads the live history so undo→edit forks cleanly. History survives restart — *nothing is ever destroyed*. |
| Workspace | **Single fixed workspace.** One database at a fixed XDG path; `leap` always opens it. |

## Storage location

`$XDG_DATA_HOME/leap/workspace.db` (via the `dirs` crate; falls back to
`~/.local/share/leap/workspace.db`). The text is user data, so it lives under the
data dir, not the cache/state dir. SQLite runs in **WAL** mode
(`synchronous=NORMAL`) so per-keystroke commits are cheap and crash-safe. The
`rusqlite` `bundled` feature compiles SQLite from source — no system dependency,
so the static musl build stays self-contained.

## The document marker

A single in-band sentinel character marks a document boundary: **U+001E RECORD
SEPARATOR** (`\x1e`) — semantically exactly a record separator, never typed by a
user, invisible to ordinary text handling. It counts as one char in the rope, so
all existing offset math (cursor, LEAP, edit-log positions) just works.

Rendering: a line whose content is the marker draws as a full-width horizontal
rule (e.g. `──────────`). The text immediately after a marker is the document's
*leading text*, used as its label for navigation. LEAP to a document = LEAP to its
leading words (no separate index; markers are just part of the stream).

`Ctrl+PgUp` / `Ctrl+PgDn` (or the prefix `C-x p` / `C-x n`) jump to the previous /
next document; bare `PgUp` / `PgDn` page a screenful. `M-Enter` ("new document")
inserts a marker on its own line and enters it. Keys are kept portable (Ctrl/Alt +
named keys, no Kitty/Hyper dependency) and avoid bracket/symbol keys, which live on
a deep layer on the author's Ergodox EZ. See [[leap-keybindings-ergodox]].

## Data model (SQLite schema)

```
meta(key TEXT PRIMARY KEY, value TEXT)               -- schema_version, …
snapshot(upto_seq INTEGER, text TEXT)                -- latest full-text checkpoint (≤1 row)
edits(                                                -- the append-only log
  seq      INTEGER PRIMARY KEY AUTOINCREMENT,
  parent   INTEGER NOT NULL,   -- seq this edit was applied after (0 = root); threads live history
  op       INTEGER NOT NULL,   -- 0 = insert, 1 = delete
  pos      INTEGER NOT NULL,   -- char index the edit starts at
  body     TEXT NOT NULL,      -- inserted text, or (for delete) the removed text, so undo can restore
  group_id INTEGER NOT NULL,   -- coalescing group for undo (a run of typing shares one)
  ts       INTEGER NOT NULL)   -- unix millis, for history/debug
state(id INTEGER PRIMARY KEY CHECK(id=1),             -- single-row resume state
  head INTEGER NOT NULL,       -- the live history tip (seq); undo moves it to parent
  cursor INTEGER, top INTEGER, left INTEGER)          -- "resume exactly where you were"
```

**Reconstruct on startup:** load the snapshot text (or empty) + replay `edits`
along the parent chain from `snapshot.upto_seq` up to `state.head`, applying each
op to a `String`, then build the rope. Cursor/scroll come from `state`.

**Undo (scrub):** undo applies the inverse of the edit at `head` in memory and
moves `head` to its `parent` (rows are never deleted). Redo moves `head` to the
child. A *new* edit while undone appends with `parent = head`, forking a fresh
branch; the orphaned tail stays on disk (nothing lost) but is off the live chain.

**Compaction:** periodically (size threshold / on clean quit) write the current
full text as a `snapshot` at `head` and delete edits at/below it that are no
longer reachable. Keeps replay bounded.

## Build sequence (branch milestones)

1. **Store** — `src/store.rs`: schema, `append(ops)`, `resume()` (snapshot +
   replay), `set_state`, `snapshot()` compaction. Unit-tested over temp/in-memory
   DBs, no TTY. ✓ done.
2. **Wire persistence** — `Buffer` journals each edit (`Vec<EditOp>`); `Editor`
   drains the journal and `append`s per key, and persists cursor/scroll state.
   Construct the buffer from `Store::resume()`. Drop file I/O + file CLI. Quit is
   instant (always saved). First run seeds the tutorial (`src/tutorial.rs`).
   ✓ done.
3. **Document markers** — the `\x1e` sentinel: `C-x C-n` insert (own line),
   full-width-rule render, `C-x [` / `C-x ]` jump, LEAP roams the stream. ✓ done
   (selection-aware deletion of a marker is future work).
4. **Log-scrub undo/redo** — `C-/` / `C-?` (or `C-x u`) walking the log via the
   parent chain, with run coalescing; survives restart. ← *next.*
5. **Polish** — compaction tuning, status line document index / position-in-stream,
   import/export escape hatch (deferred), tree-sitter highlighting.

## Verification

- `cargo test` — store round-trips (append→resume), delete replay, snapshot
  compaction, state persistence, marker navigation, undo scrub across a reopen.
- `cargo clippy --all-targets` clean.
- PTY end-to-end: type text, kill the process (no quit), relaunch → text + cursor
  exactly restored; insert a document boundary, jump between documents, LEAP to a
  document by its leading text; undo across a restart.
