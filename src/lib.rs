#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod administration;
pub mod app;
pub mod billing;
pub mod catalog;
pub mod config;
pub mod contract;
pub mod discovery;
pub mod http;
pub mod identity;
pub mod infra;
pub mod matches;
pub mod media;
pub mod moderation;
pub mod notifications;
pub mod operations;
pub mod outbox;
pub mod privacy;
pub mod profiles;
pub mod reports;
pub mod shared;
