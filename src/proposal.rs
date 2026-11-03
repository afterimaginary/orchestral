//! `orchestral proposal-done <name>`: the end of a worker's design
//! proposal, once the owner chose a design or rejected it.
//!
//! A worker spawned with `--proposal` is on its proposal from its start until
//! this command. `status` shows what proposals spend, since variants add real
//! cost: each open one so far, and this run's finished ones, which this
//! command adds to.

use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::budget::Estimate;
use crate::gate;
use crate::outcome::Failure;
use crate::sessions;
use crate::state::Store;

#[derive(Debug, Serialize)]
pub struct Done {
    pub name: String,
    /// What the proposal used, or null when its transcript couldn't be read.
    pub used: Option<Estimate>,
}

impl fmt::Display for Done {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.used {
            Some(used) => write!(f, "{}'s proposal is done. It used {used}.", self.name),
            None => write!(
                f,
                "{}'s proposal is done. Its usage couldn't be read, so it isn't counted.",
                self.name
            ),
        }
    }
}

pub fn done(cwd: &Path, name: &str) -> Result<Done, Failure> {
    let store = Store::for_repo(cwd)?;
    let state = store.read()?;
    if !state.proposals.contains_key(name) {
        return Err(Failure::Refused {
            reason: format!("{name} isn't on a design proposal"),
            fix: "name a worker spawned with --proposal whose proposal isn't done".into(),
        });
    }
    // The proposal ends either way: an unreadable usage isn't counted, rather
    // than holding up the owner's choice.
    let used = sessions::agents(Path::new("claude"))
        .and_then(|agents| gate::proposal_usage(&state, name, &agents))
        .ok()
        .flatten();
    let tokens = used.as_ref().map_or(0, |used| used.estimated_tokens);
    store.update(|state| {
        if state.proposals.remove(name).is_some() {
            state.proposal_tokens += tokens;
        }
    })?;
    Ok(Done {
        name: name.to_owned(),
        used,
    })
}
