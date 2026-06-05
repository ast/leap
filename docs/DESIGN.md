# leap — Design

An experimental terminal text editor: the **Canon Cat's** modeless, LEAP-driven
interaction model with **Emacs**-style conveniences. Fast, multithreaded, and
portable as a static musl binary.

## Decisions (locked)

| Area | Decision |
|------|----------|
| Editing model | **Modeless** — always inserting text; commands via modifier chords + LEAP keys. No vi-style modes. |
| Document model | **Single buffer** — one document fills the screen, Cat-style. |
| Configuration | **Compile-time only** — themes, keybindings, options are Rust consts + cargo features. No runtime config parser. |
| Privileges | **Normal permissions** — edit any file the user can already read/write; no sudo/escalation. |
| Navigation | **Cat incremental LEAP** — type characters and the cursor flies to the next/previous occurrence live. |
| Selection | **Cat LEAP-span** — leap to one end, leap-with-select to the other; the span between becomes the selection. |
| Undo | **Linear undo/redo with grouping** — consecutive typing/deletes coalesce into one step. |
| Confirmations | **No modal yes/no dialogs** (Raskin). Destructive actions use a **hold-to-confirm gesture** (`src/hold.rs`): sustained key pressure + meter, abortable by release. Quit discards via **hold `C-q`**. Kitty-protocol release events with a repeat-timeout fallback. |
| File open/save | **fzf-style finder overlay** (`src/finder.rs`, `src/walk.rs`) — no modal minibuffer. A visible, escapable transient context: gitignore-aware walk from repo-root, nucleo fuzzy-match, type-to-narrow. Save mode = typed query is the path. Goto-line dropped (LEAP supersedes). |
| Concurrency | Async syntax highlighting, async file I/O, background search; instant startup. |
| Languages | tree-sitter grammars bundled: **Rust, C, C++, TOML, JSON, YAML, Markdown**. |
| Primary target | **x86_64-unknown-linux-musl** (static). Keep code portable so FreeBSD stays compilable. |

## Screen layout

```
┌─────────────────────────────────────────┐
│ text area (single document, scrolling)   │
│ …                                        │
│ …                                        │
├─────────────────────────────────────────┤  ← status line (filename ● | lang | ln:col | %)
│ echo line (display-only)                 │  ← transient messages, hold meter
└─────────────────────────────────────────┘
```

- **Status line** (Cat ruler / Emacs mode line): file name + modified dot, language,
  cursor `line:col`, char offset / `%` through file, selection size, encoding/EOL,
  and a LEAP-active indicator.
- **Echo line** (bottom-most, `src/echo.rs`): **display-only** — transient messages
  (Saved, read-only) and the hold-`C-q` meter. Never an input prompt (no modal
  minibuffer — Raskin). File open/save happens in the fzf finder overlay; LEAP
  query displays in the LEAP context.

## Architecture

Well-structured, message-passing concurrency. The **main thread owns the buffer and
does all input + rendering and never blocks**; workers receive cheap `ropey`
snapshots (O(1) `Arc`-shared clones) and return results tagged with a buffer
**version**; stale results (version moved on) are discarded.

```
main thread ──┬── crossterm event read → keymap → command dispatch → edit buffer
              └── render (viewport → screen cells)
                      ▲ results (tagged w/ version)
   ┌──────────────────┼───────────────────────────┐
   │ highlight worker │ search worker │ io worker  │   (std::thread + channels)
   │ tree-sitter on   │ LEAP match on │ load/save  │
   │ rope snapshot    │ big buffers   │ large files│
   └──────────────────┴───────────────┴────────────┘
```

### Module map (`src/`)

- `main.rs` — clap CLI, terminal raw-mode/alt-screen setup+teardown, bootstrap.
- `editor.rs` — top-level `Editor` state + the event loop; orchestrates everything.
- `buffer/` — `Buffer` type wrapping a `ropey::Rope` **plus** cursor, selection,
  undo, dirty flag, path, encoding/EOL. ropey stays an implementation detail here.
  - `edit.rs` — insert/delete/replace primitives, version counter.
  - `undo.rs` — linear undo/redo stack with coalescing.
  - `cursor.rs` — position + LEAP-span selection + coordinate conversion.
- `leap.rs` — incremental LEAP engine (forward/back, creep), drives background search.
- `view.rs` — viewport, scrolling, buffer-coords → screen-cells, unicode width.
- `render.rs` — crossterm drawing of text area, status line, echo line.
- `syntax.rs` — tree-sitter parse + highlight, background worker, snapshot-based.
- `theme.rs` — compile-time `Theme` structs; built-ins; tree-sitter capture → color.
- `keymap.rs` — compile-time key → `Command` table; modeless dispatch.
- `command.rs` — `Command` enum + execution.
- `echo.rs` — display-only bottom message line (no input mode).
- `finder.rs` + `walk.rs` — fzf-style file open/save overlay over a gitignore-aware walk.
- `statusline.rs` — status line model + formatting.
- `fileio.rs` — async load/save; atomic save (temp + rename); EOL/UTF-8 detection.
- `clipboard.rs` — internal kill buffer + OS clipboard via **OSC 52** (no X deps;
  works over SSH and on headless static builds).

### Versioning for async correctness

Every edit bumps a monotonic `version: u64`. A worker is handed `(snapshot, version)`;
its result carries that version. The main thread applies a result only if
`result.version == buffer.version`, else discards (a newer edit already happened).
No locks on the buffer; only immutable snapshots cross thread boundaries.

## The LEAP mechanism (the heart of the editor)

Cat semantics: press a LEAP key, type the target text, and the cursor moves to the
next occurrence **as each character is typed**; pressing LEAP again jumps to the
following occurrence; landing commits, escape cancels and returns to origin.
Two LEAP keys = leap-backward and leap-forward. A LEAP tap with no text = "creep"
one character. Selection = leap to start, then leap-with-select to the end.

**Terminal constraint (important):** classic terminals send only key *press* events,
no key *release* and no modifier hold-state — so a literal "hold LEAP while typing"
isn't detectable everywhere. Plan:

- **Primary model — LEAP session (works on every terminal):** a LEAP key opens an
  incremental search session (Emacs `isearch` feel); each typed char advances the
  cursor to the next match, the LEAP key repeats, Enter lands, Esc cancels. This is
  functionally identical to the Cat in practice and is the baseline implementation.
- **Enhanced model (opt-in):** where the **Kitty keyboard protocol** is available
  (kitty, foot, WezTerm, recent alacritty — via crossterm's
  `PushKeyboardEnhancementFlags`), use real key release / hold reporting to offer
  true hold-to-LEAP. Detected at startup; falls back to the session model.

## Themes (compile-time)

`Theme` struct: editor fg/bg, selection, LEAP-match highlight, status line colors,
and a map from tree-sitter highlight capture names (`@keyword`, `@string`,
`@function`, …) to colors (RGB via crossterm `Color`). A few built-in themes as
`const`s; the active theme is chosen by a **cargo feature** (e.g.
`--features theme-gruvbox`), defaulting to one built-in. No runtime theme loading.

## Tree-sitter

Grammars compiled in via their crates (`tree-sitter-rust`, `-c`, `-cpp`, `-toml`,
`-json`, `-yaml`, `-md`). Language picked by file extension. Bundle each grammar's
`highlights.scm`. Incremental: keep the `Tree`, feed `InputEdit` on each change,
re-parse against the old tree on the **highlight worker**, map captures → theme.

## Crates

`crossterm` (TUI), `clap` + `clap_complete` (CLI + completions), `anyhow`
(top-level errors), `thiserror` (module error types), `ropey` (buffer),
`tree-sitter` + grammar crates, `crossbeam-channel` (worker messaging),
`unicode-width` + `unicode-segmentation` (column width, graphemes).

## CLI (clap)

`leap [FILE]` with `--line N` (open at line), `--readonly`, `--version`, and a
hidden `completions <shell>` subcommand (clap_complete).

## Build sequence (milestones)

1. **Skeleton** — raw mode + alt screen, event loop, draw empty buffer, quit; clap file arg.
2. **Buffer + view** — rope insert/delete, cursor movement, viewport scrolling, render.
3. **Chrome** — status line + display-only echo line.
4. **Files** — open/save (sync), atomic save, modified flag, EOL/UTF-8 handling.
5. **LEAP** — incremental LEAP session navigation (`C-s`/`C-r`, lands at match start, wraps). ✓ done.
6. **Selection + clipboard** — LEAP-span selection, cut/copy/paste, kill buffer + OSC 52.
7. **Undo/redo** — linear stack with coalescing.
8. **fzf finder** — file open/save overlay (replaces the minibuffer prompts).
9. **Highlighting** — tree-sitter, sync first.
10. **Concurrency** — move highlighting, search, and I/O to workers; version tagging.
11. **Themes** — compile-time `Theme` + cargo features.
12. **Portability** — static musl build, clap_complete output, verify FreeBSD compiles.

## Testing / verification

The TUI takes over the terminal, so push logic **out of the render path** and unit
test it without a TTY: rope edits, coordinate conversion, LEAP matching, undo
coalescing, selection spans, EOL detection. `cargo test` covers these. Manual
end-to-end runs (`cargo run -- <file>`) verify rendering, LEAP feel, and theming.
Keep the static build honest: `cargo build --release --target x86_64-unknown-linux-musl`.

## Open / deferred (not in v1)

- Multiple buffers, split windows — explicitly out (single-buffer decision).
- Runtime config & scripting (elisp-like) — out (compile-time decision).
- Collaborative editing / CRDT — out (single-user; see buffer decision).
- Privilege escalation for root files — out (launch under doas/sudo instead).
