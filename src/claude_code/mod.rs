//! Claude Code integration module.
//!
//! This module provides direct integration with Claude Code's native
//! file-based memory, rules, hooks, and skills systems. Instead of
//! relying on MCP as the primary integration path, Nellie writes
//! directly to Claude Code's file paths so that context loads
//! automatically at session start.
//!
//! # Sub-modules
//!
//! - [`paths`]: Path resolution for Claude Code directories
//!   (`~/.claude/projects/`, `~/.claude/rules/`, etc.)
//! - [`memory_writer`]: Atomic writes of individual memory `.md`
//!   files with YAML frontmatter
//! - [`memory_index`]: MEMORY.md index manager with line budget
//!   enforcement and `[nellie]` tagging
//! - [`mappers`]: Converters from Nellie records (`LessonRecord`,
//!   etc.) to Claude Code memory files and index entries
//! - [`sync`]: Full sync command orchestrating lessons and checkpoints
//!   into Claude Code memory files
//! - [`rules`]: Conditional rules generator mapping lesson tags to
//!   file glob patterns for context-aware rule loading
//! - [`transcript`]: JSONL transcript parser for Claude Code session
//!   files, converting raw events into structured entries
//! - [`extractor`]: Pattern extractor for passive learning — detects
//!   corrections, tool failures, repeated patterns, explicit saves,
//!   and build failures in parsed transcripts
//! - [`ingest`]: Transcript ingestion pipeline orchestrating parsing,
//!   extraction, deduplication, and storage of lessons
//! - [`hooks`]: Claude Code session hooks management for integrating
//!   Nellie sync and ingest into settings.json
//! - [`dedup`]: Memory deduplication using semantic similarity to
//!   prevent memory bloat from near-duplicate lessons
//! - [`daemon`]: Background transcript watcher that monitors
//!   `~/.claude/projects/` for completed session transcripts

pub mod daemon;
pub mod dedup;
pub mod extractor;
pub mod hooks;
pub mod ingest;
pub mod inject;
pub mod mappers;
pub mod memory_index;
pub mod memory_writer;
pub mod paths;
pub mod remote;
pub mod rules;
pub mod sync;
pub mod transcript;
