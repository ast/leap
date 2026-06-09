//! The seed content for a fresh workspace.
//!
//! On the Canon Cat the manual lived *inside* the workspace — it was just text
//! you could read, edit, leap through, or delete. We do the same: a brand-new
//! database is seeded with [`TUTORIAL`] as its first content (one persisted
//! insert), so the very first thing you see explains how to drive the editor.
//! It's ordinary editable text — change it, or LEAP past it and start writing.

/// `\u{1e}` (RECORD SEPARATOR) is the document-boundary marker — see
/// [`crate::buffer::Buffer::DOC_MARKER`]. It splits the tutorial into two
/// documents so `C-x [` / `C-x ]` have something to jump between.
pub const TUTORIAL: &str = "\
Welcome to leap — the Canon Cat edition.

There are no files here. This whole screen is one continuous stream of
text that IS your workspace, kept in a small database. Everything you type
is saved instantly, so you can quit any time and come right back to exactly
where you were — cursor and all. There is no \"save\" command; there is
nothing to lose.

MOVING AROUND (no arrow keys needed)
  C-f / C-b    forward / back one character
  C-n / C-p    next / previous line
  C-a / C-e    start / end of line
  M-f / M-b    forward / back one word
  PgUp / PgDn  up / down a screenful  (also C-v / M-v)
  C-l          redraw, centering the cursor's line

LEAP — the fastest way to move
  C-s          leap forward: just type what you want to fly to
  C-r          leap backward
  As you type, the cursor flies to the next match. Press C-s / C-r again to
  jump to the following one. Enter lands; Esc returns to where you started.
  Try it: press C-s and type  workspace

EDITING
  Type            insert text
  C-h / Backspace delete the character before the cursor
  C-d             delete the character at the cursor
  C-k             kill to end of line     C-w  kill the word before the cursor
  C-y             yank (paste) what you killed

SELECTING (the Cat's LEAP-span)
  C-Space         drop a mark, then move or LEAP — the span lights up
  C-w  cut    M-w  copy    C-y  paste    Backspace / C-d  erase the selection
  Calc works on a selection too: select  3 * 14  then press  M-c

CALC — the Cat's built-in calculator
  Type an arithmetic expression on a line, e.g.   12 * (3 + 4)
  then press  M-c  (or M-=) and the answer is written in place:
      12 * (3 + 4) = 84
  Works with + - * / %, parentheses, and decimals. Run it again to recompute.

DOCUMENTS
  Your stream is divided into documents by a boundary marker, drawn as the
  rule below. Jump between documents with:
  Ctrl+PgUp / Ctrl+PgDn   previous / next document  (or C-x p / C-x n)
  M-Enter                 start a NEW document here (drops a fresh boundary)
  Or just LEAP to a document by typing a word from its first line.

QUITTING
  C-q          quit. (Your work is already saved — nothing is discarded.)

Leap down to the next document with  Ctrl+PgDn  — a scratch page waits.
\u{1e}
Scratch document.

This page is empty and yours. Write anything here.

To prove the Cat's promise: type a few words, quit with C-q, then start
leap again — you'll be right back here with the cursor where you left it.

To start yet another document, press  M-Enter.
";
