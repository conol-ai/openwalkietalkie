//! `cindy` on the deck: not written yet.
use super::Orchestrator;
use crate::config::Config;

/// TODO(feat/orchestrator-cindy): the cindy adapter (`docs/orchestrator-adapters.md`).
pub fn new(_cfg: &Config) -> Option<Box<dyn Orchestrator>> {
    None
}
