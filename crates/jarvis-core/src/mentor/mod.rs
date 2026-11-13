//! Mentor mode: JARVIS teaches the user by voice while they build a project
//! (see docs/superpowers/specs/2026-11-13-mentor-mode-design.md).

// The parts are wired into the session step by step (plan tasks 4-9).
#![allow(dead_code)]

pub mod commands;
pub(crate) mod protocol;
pub(crate) mod snapshot;
