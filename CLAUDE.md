# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project vision

`leap` is an experimental terminal text editor in the spirit of the **Canon Cat** and **Emacs**. Design intent (from the original brief):

- Syntax highlighting via **tree-sitter**.
- Deliberate choice of the "best" data structure for editable buffers (rope / piece table / gap buffer — to be decided and documented when chosen).
- Built on **crossterm** (terminal UI), **clap** + **clap_complete** (CLI / shell completions), **anyhow** (application error handling), **thiserror** (library error types).
- Should build as a **static musl binary** for portability (`x86_64-unknown-linux-musl`).
- `init.el` (the author's Emacs config, ~1200 lines) is kept in the repo as a reference for desired keybindings/behaviors — it is **not** part of the build.
- Canon Cat manual should live in a `docs/` folder for design reference.

## Design (decided)

Full design and build sequence: **`docs/DESIGN.md`** (read it before implementing). Locked decisions:

- **Modeless, single-buffer** editor (Canon Cat model + Emacs conveniences). No vi-style modes, no splits.
- **Compile-time configuration** — themes and keybindings are Rust consts + cargo features (e.g. `--features theme-gruvbox`). No runtime config parser.
- **Buffer** = `ropey` rope wrapped in a `Buffer` type that also owns cursor, LEAP-span selection, linear undo (with coalescing), dirty flag, path. Single-user, no CRDT.
- **Navigation** = incremental **LEAP** (type to fly the cursor to matches). Primary impl is an isearch-style "LEAP session" (works on all terminals); true hold-to-LEAP is opt-in via the Kitty keyboard protocol where available.
- **Concurrency** — main thread owns the buffer and never blocks; workers get O(1) `ropey` snapshots tagged with a `version: u64`; stale results discarded. Async syntax highlighting, file I/O, and search.
- **tree-sitter** grammars bundled: Rust, C, C++, TOML, JSON, YAML, Markdown (by file extension).
- **Clipboard** via internal kill buffer + **OSC 52** (no X deps; works over SSH / static musl).
- Primary build target **x86_64-unknown-linux-musl**; keep code portable so FreeBSD stays compilable.

## Current state

Greenfield. `src/main.rs` is still the `cargo new` stub and `Cargo.toml` has no dependencies yet. Implement against `docs/DESIGN.md`'s module map and milestone sequence; update both files as real structure lands.

## Commands

- Build: `cargo build`
- Run: `cargo run`
- Test: `cargo test`
- Run a single test: `cargo test <test_name>` (substring match on test function names)
- Lint: `cargo clippy --all-targets`
- Format: `cargo fmt`
- Static musl release binary: `cargo build --release --target x86_64-unknown-linux-musl` (requires `rustup target add x86_64-unknown-linux-musl`)

## Notes

- Edition is **2024** — use current Rust idioms.
- The TUI takes over the terminal (raw mode / alternate screen via crossterm), so `cargo run` is interactive; prefer `cargo test` and `cargo clippy` for non-interactive verification.
