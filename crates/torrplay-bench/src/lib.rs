// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The milestone 5 benchmark harness, following the benchmark protocol of
//! `docs/acceptance.md`.
//!
//! [`fixtures`] writes the generated content and torrents that announce to
//! the local [`tracker`], [`seed`] serves them from local peers, and
//! [`runner`] replays an access [`trace`] against an implementation behind a
//! [`target::Target`], measuring each request with [`player`] and the server
//! process with [`sampler`], recording its [`environment`]. [`summary`]
//! reports distributions across runs.

pub mod compare;
pub mod environment;
pub mod fixtures;
pub mod player;
pub mod runner;
pub mod sampler;
pub mod seed;
pub mod summary;
pub mod target;
pub mod trace;
pub mod tracker;
