//! `orchestral limits`: the plan's usage windows.

use std::fmt;

use serde::Serialize;

/// The plan's usage windows, or why they can't be read.
#[derive(Serialize)]
pub struct Limits {
    pub available: bool,
    /// Why the limits are unavailable, when they are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl fmt::Display for Limits {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.reason {
            Some(reason) => write!(f, "Plan limits are unavailable: {reason}."),
            None => write!(f, "Plan limits are available."),
        }
    }
}

pub fn read() -> Limits {
    Limits {
        available: false,
        reason: Some("reading them isn't implemented yet".into()),
    }
}
