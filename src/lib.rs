//! Cœur de aur-guard, partagé par les frontends CLI, TUI et GUI.

pub mod ai;
pub mod aur;
pub mod config;
pub mod i18n;
pub mod pipeline;
pub mod scan;

#[cfg(feature = "tui")]
pub mod tui;
