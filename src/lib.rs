//! Asqu: version control that nobody has to manage.
//!
//! Every project gets one hidden store at `.asqu/store.db`, created on first use.

pub mod aitools;
pub mod backup;
pub mod compare;
pub mod config;
pub mod decide;
pub mod event;
pub mod describe;
pub mod receipt;
pub mod risk;
pub mod run;
pub mod serve;
pub mod setup;
pub mod sign;
pub mod tools;
pub mod hash;
pub mod hub;
pub mod platform;
pub mod projects;
pub mod scan;
pub mod see;
pub mod store;
pub mod tidy;
pub mod tries;
pub mod update;
pub mod trust;
pub mod watch;
pub mod when;
pub mod worktree;

pub use store::{SaveReport, Store, Why};
