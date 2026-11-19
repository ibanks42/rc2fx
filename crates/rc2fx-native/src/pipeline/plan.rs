//! The launch plan the pipeline is built from, and the registry of the
//! mixer's ports it binds.
use super::FRAMES;
use rc2fx_core::{Target, TargetKind};
use rc2fx_native_control::{MAX_PATHS, PathMode};
use rc2fx_suppression::{PRODUCTION_WORKER, WorkerOptions};
use serde::Deserialize;
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Input,
    Output,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Plan {
    pub(super) version: u32,
    pub(super) sample_rate: u32,
    pub(super) frames: usize,
    /// Tests only: lets an unprivileged fixture run its worker without the
    /// production policy. A parsed plan never has one (a `worker` key is
    /// refused as unknown), so production always uses `PRODUCTION_WORKER`.
    #[cfg(test)]
    #[serde(skip)]
    pub(super) worker: Option<WorkerPlan>,
    pub paths: Vec<PathPlan>,
}
#[cfg(test)]
pub(super) struct WorkerPlan {
    pub(super) cpu_mask: Option<u64>,
    pub(super) realtime_priority: Option<u8>,
    pub(super) nice: Option<i8>,
}
impl Plan {
    #[cfg(not(test))]
    pub(super) fn worker_options(&self) -> WorkerOptions {
        PRODUCTION_WORKER
    }
    /// The fixture's worker, when it sets one.
    #[cfg(test)]
    pub(super) fn worker_options(&self) -> WorkerOptions {
        self.worker
            .as_ref()
            .map_or(PRODUCTION_WORKER, |worker| WorkerOptions {
                cpu_mask: worker.cpu_mask,
                realtime_priority: worker.realtime_priority,
                nice: worker.nice,
            })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathPlan {
    pub direction: Direction,
    pub ports: Vec<String>,
    pub target: Target,
    pub mode: PathMode,
    /// An output heard only on the mixer's own hardware outputs.
    pub hardware: bool,
}
impl Plan {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let plan: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if plan.version != 1
            || plan.sample_rate != 48_000
            || plan.frames != FRAMES
            || plan.paths.len() > MAX_PATHS
        {
            return Err("unsupported native plan geometry/count".into());
        }
        let mut ids = HashSet::new();
        let mut ports = HashSet::new();
        let mut suppressors = 0;
        for path in &plan.paths {
            path.target.validate().map_err(|error| error.to_string())?;
            if path.target.suppression.enabled {
                suppressors += 1;
                let mic = path.direction == Direction::Input
                    && path.target.kind == TargetKind::Mic
                    && path.target.channels == 1;
                let chat = path.direction == Direction::Output
                    && path.target.is_chat_output()
                    && !path.hardware;
                if suppressors > 1 || !(mic || chat) {
                    return Err("native plan admits one mono microphone or the Chat voice mix with suppression".into());
                }
            }
            if path.ports.len() != path.target.channels as usize
                || (path.hardware && path.direction != Direction::Output)
                || !ids.insert(&path.target.id)
                || (path.direction == Direction::Output) != (path.target.kind == TargetKind::Output)
            {
                return Err("native plan target identity/direction mismatch".into());
            }
            for port in &path.ports {
                if port.is_empty()
                    || port.len() > 64
                    || port.contains([':', '\0'])
                    || !ports.insert((path.direction as u8, port))
                {
                    return Err("native plan duplicate/invalid port".into());
                }
            }
        }
        Ok(plan)
    }
}

pub struct Pin {
    pub client: usize,
    pub pointer: usize,
    pub name: String,
    pub direction: Direction,
    pub alive: AtomicBool,
}
#[derive(Default)]
pub struct Registry {
    pub ports: Vec<Arc<Pin>>,
}
impl Registry {
    pub fn register(&mut self, client: usize, pointer: usize, name: &[u8], flags: usize) -> bool {
        // Invalidate the old identity even if the new registration cannot be
        // tracked. A full registry or invalid replacement must not leave an old
        // pipeline attached to a pointer now owned by a different port.
        self.retire(client, pointer);
        let direction = match flags & 3 {
            1 => Direction::Input,
            2 => Direction::Output,
            _ => return false,
        };
        let Ok(name) = std::str::from_utf8(name) else {
            return false;
        };
        if name.is_empty() || name.len() > 64 || self.ports.len() >= 512 {
            return false;
        }
        self.ports.push(Arc::new(Pin {
            client,
            pointer,
            name: name.into(),
            direction,
            alive: AtomicBool::new(true),
        }));
        true
    }
    pub fn retire(&mut self, client: usize, pointer: usize) {
        self.ports.retain(|pin| {
            if pin.client == client && pin.pointer == pointer {
                pin.alive.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
    }
    pub fn close(&mut self, client: usize) {
        self.ports.retain(|pin| {
            if pin.client == client {
                pin.alive.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
    }
}
