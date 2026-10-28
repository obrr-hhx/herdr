//! Bounded background history reads. No filesystem, subprocess or SSH work runs in
//! shell composition/input; completion is fenced to endpoint boot and view generation.
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use super::endpoint::{ClientEndpointId, EndpointCatalog};
use crate::agent_history::HistoryPage;

pub(crate) struct HistoryResult {
    pub endpoint: ClientEndpointId,
    pub boot: String,
    pub generation: u64,
    pub older: bool,
    pub result: Result<HistoryPage, String>,
}

struct Job {
    endpoint: ClientEndpointId,
    boot: String,
    generation: u64,
    older: bool,
    rx: Receiver<Result<HistoryPage, String>>,
}

type ReaderCache = Arc<Mutex<HashMap<String, Arc<crate::remote::ClaudeHistoryReader>>>>;
#[derive(Default)]
pub(super) struct HistoryJobs {
    jobs: Vec<Job>,
    readers: ReaderCache,
}

impl HistoryJobs {
    pub(super) fn start(
        &mut self,
        endpoint: ClientEndpointId,
        boot: String,
        generation: u64,
        pane: String,
        before: Option<u64>,
        known: String,
    ) -> bool {
        if self.jobs.len() >= 4 {
            return false;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let source = endpoint.clone();
        let readers = Arc::clone(&self.readers);
        std::thread::spawn(move || {
            let result =
                read(&source, &pane, before, &known, &readers).map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        self.jobs.push(Job {
            endpoint,
            boot,
            generation,
            older: before.is_some(),
            rx,
        });
        true
    }

    pub(super) fn poll(&mut self) -> Vec<HistoryResult> {
        let mut output = Vec::new();
        self.jobs.retain(|job| {
            let result = match job.rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => Err("history worker stopped".into()),
            };
            output.push(HistoryResult {
                endpoint: job.endpoint.clone(),
                boot: job.boot.clone(),
                generation: job.generation,
                older: job.older,
                result,
            });
            false
        });
        output
    }
}

fn read(
    endpoint: &ClientEndpointId,
    pane: &str,
    before: Option<u64>,
    known: &str,
    readers: &ReaderCache,
) -> std::io::Result<HistoryPage> {
    match endpoint {
        ClientEndpointId::Ssh(id) => {
            let profiles = EndpointCatalog::load_profiles().map_err(std::io::Error::other)?;
            let profile = profiles
                .into_iter()
                .find(|p| &p.id == id && p.enabled)
                .ok_or_else(|| std::io::Error::other("history endpoint was removed"))?;
            read_remote(
                &profile.target,
                &profile.session,
                pane,
                before,
                known,
                readers,
            )
        }
        ClientEndpointId::Local => {
            if let (Ok(target), Ok(session)) = (
                std::env::var(crate::agent_history::REMOTE_TARGET_ENV),
                std::env::var(crate::agent_history::REMOTE_SESSION_ENV),
            ) {
                read_remote(&target, &session, pane, before, known, readers)
            } else {
                let session = crate::session::active_name().unwrap_or_else(|| "default".into());
                crate::agent_history::read_local(&session, pane, before, known)
            }
        }
    }
}

fn read_remote(
    target: &str,
    session: &str,
    pane: &str,
    before: Option<u64>,
    known: &str,
    readers: &ReaderCache,
) -> std::io::Result<HistoryPage> {
    let reader = {
        let mut cache = readers
            .lock()
            .map_err(|_| std::io::Error::other("history source cache unavailable"))?;
        if !cache.contains_key(target) {
            if cache.len() >= 4 {
                cache.clear();
            }
            cache.insert(
                target.to_owned(),
                Arc::new(crate::remote::ClaudeHistoryReader::new(target)?),
            );
        }
        cache
            .get(target)
            .cloned()
            .ok_or_else(|| std::io::Error::other("history source unavailable"))?
    };
    reader.read(session, pane, before, known)
}
