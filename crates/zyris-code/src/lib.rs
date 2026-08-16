//! zyris-code — a terminal client for talking with the Attacca agent.
//!
//! Separates the pure core from I/O. Frame conversion, timeline, markdown, layout, scroll, and
//! input are all pure functions covered by unit tests; the network and the terminal live only in
//! two places: `conn` and `app::run()`.
//!
//! Integration tests (`tests/`) can't see the binary, so the modules are made public here.

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod command;
pub mod config;
pub mod conn;
pub mod enroll;
pub mod event;
pub mod github;
pub mod githubform;
pub mod hooks;
pub mod input;
pub mod instructions;
pub mod lang;
pub mod markdown;
pub mod mcp;
pub mod mode;
pub mod newproject;
pub mod notice;
pub mod panel;
pub mod picker;
pub mod plugin;
pub mod print;
pub mod question;
pub mod repo;
pub mod rows;
pub mod scroll;
pub mod selection;
pub mod term;
pub mod theme;
pub mod timeline;
pub mod todos;
pub mod tool_view;
pub mod tools;
pub mod undo;
pub mod update;
pub mod usage;
pub mod widgets;
