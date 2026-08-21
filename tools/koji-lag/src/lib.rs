// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Quantify Koji build queue lag and per-arch build-time drag.
//!
//! Fedora's primary architectures build in lockstep, so one slow
//! or queue-starved architecture delays every build — and scratch
//! builds, which gate dist-git PR CI, run at lower priority still.
//! This crate sweeps Koji task metadata (via anonymous hub
//! XML-RPC), stores it in a mergeable dataset so independently
//! collected runs can be pooled, and reports per-arch queue-wait /
//! build-time distributions plus critical-path attribution (which
//! arch finished last, and how much later than the runner-up).

pub mod annotate;
pub mod class;
pub mod csv;
pub mod dataset;
pub mod events;
pub mod export;
pub mod fetch;
pub mod health;
pub mod instance;
pub mod periods;
pub mod pool;
pub mod probe;
pub mod rebuild;
pub mod report;
pub mod schedule;
pub mod stall;
pub mod stats;
pub mod store;
pub mod sweep;
pub mod sync;
