// Adapted from app/src/updateEverything{.logic,Run}.ts and useUpdateEverything.ts.
// The upstream T3 MIT notice is recorded in app/t3/provenance.json.
#[cfg(not(any(feature = "conversation", feature = "workflows", feature = "hub")))]
use super::date;
use super::{api, discard, kit::*, machines, poll, restore, save, uuid, Query};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
#[cfg(not(any(feature = "conversation", feature = "workflows", feature = "hub")))]
use wasm_bindgen::JsValue;

const RUN_KEY: &str = "minarbot:update-everything";
const LOCAL_KEY: &str = "minarbot:update";
const TIMEOUT: f64 = 15.0 * 60_000.0;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Source {
    #[serde(rename = "GitHub")]
    Github,
    #[serde(rename = "Local feed")]
    Feed,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Feed => "Local feed",
        }
    }
}

#[derive(Clone, PartialEq, Deserialize)]
struct Release {
    id: String,
    version: String,
    notes: String,
    created: String,
    commit: String,
    source: Option<Source>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Version {
    version: String,
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Download {
    source: Option<Source>,
    bytes_transferred: Option<u64>,
    total_bytes: Option<u64>,
}

impl Download {
    fn label(&self) -> String {
        let percent = self
            .total_bytes
            .filter(|total| *total > 0)
            .map_or(0, |total| {
                (u128::from(self.bytes_transferred.unwrap_or_default()) * 100 / u128::from(total))
                    .min(100)
            });
        if percent == 0 {
            "Downloading".into()
        } else {
            format!("Downloading {percent}%")
        }
    }
}

#[derive(Clone, PartialEq, Deserialize)]
struct Attempt {
    id: String,
    version: String,
    status: String,
    reason: Option<String>,
    #[serde(flatten)]
    download: Download,
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    current: Release,
    previous: Option<Release>,
    available: Option<Release>,
    feed: Option<Release>,
    github: Option<Release>,
    github_error: Option<String>,
    last_attempt: Option<Attempt>,
    launcher_available: bool,
    running_turns: u64,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Transfer {
    state: String,
    #[serde(flatten)]
    download: Download,
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Peer {
    machine: String,
    available: Value,
    current: Option<Version>,
    launcher_available: Option<bool>,
    running_turns: Option<u64>,
    reason: Option<String>,
    last_attempt: Option<Attempt>,
    fan_out_attempt: Option<Transfer>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct Peers {
    machines: Vec<Peer>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    id: String,
    name: String,
    current: String,
    target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<Source>,
    running: u64,
    local: bool,
    skipped: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt_id: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Local {
    id: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<Source>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Command {
    #[serde(flatten)]
    request: Local,
    machines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<f64>,
    #[serde(default)]
    submitted: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Run {
    rows: Vec<Target>,
    commands: Vec<Command>,
}

#[derive(Deserialize)]
struct Receipt {
    machine: String,
    status: String,
    reason: Option<String>,
    id: Option<String>,
}
#[derive(Deserialize)]
struct Accepted {
    id: String,
}
#[derive(Deserialize)]
struct FanOut {
    machines: Vec<Receipt>,
}

fn label(status: &str) -> &str {
    match status {
        "waiting" => "Waiting",
        "submitting" | "accepted" => "Preparing update",
        "downloading" => "Downloading",
        "uncertain" => "Checking status",
        "reconnecting" => "Reconnecting",
        "backing-up" => "Backing up workspace",
        "starting" => "Restarting",
        "verifying" => "Verifying server health",
        "restoring" => "Restoring workspace snapshot",
        "done" => "Updated",
        "rolled-back" => "Rolled back",
        "failed" => "Update failed",
        "unconfirmed" => "Update unconfirmed",
        _ => status,
    }
}

fn terminal(status: &str) -> bool {
    matches!(status, "done" | "rolled-back" | "failed")
}

fn valid_id(id: &str) -> bool {
    (1..=36).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn reconnecting(message: &str) -> bool {
    message.starts_with("Minarbot connection lost.")
        || message.starts_with("Minarbot is restarting.")
}

fn version(value: &str) -> Option<([&str; 3], bool)> {
    let (base, stable) = match value.split_once('-') {
        Some((base, suffix)) if !suffix.is_empty() => (base, false),
        Some(_) => return None,
        None => (value, true),
    };
    let parts: [&str; 3] = base.split('.').collect::<Vec<_>>().try_into().ok()?;
    if parts
        .iter()
        .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    Some((
        parts.map(|part| {
            let trimmed = part.trim_start_matches('0');
            if trimmed.is_empty() {
                "0"
            } else {
                trimmed
            }
        }),
        stable,
    ))
}

fn newer(candidate: &str, current: &str) -> bool {
    let (Some((candidate, stable)), Some((current, current_stable))) =
        (version(candidate), version(current))
    else {
        return false;
    };
    (candidate.map(|part| (part.len(), part)), stable)
        > (current.map(|part| (part.len(), part)), current_stable)
}

impl Status {
    fn offers(&self) -> Vec<Release> {
        let Some(primary) = &self.available else {
            return Vec::new();
        };
        let other = if primary.source == Some(Source::Github) {
            &self.feed
        } else {
            &self.github
        };
        std::iter::once(primary)
            .chain(
                other
                    .iter()
                    .filter(|offer| newer(&offer.version, &self.current.version)),
            )
            .cloned()
            .collect()
    }
    fn feed(&self) -> Option<&Release> {
        let local = self.feed.as_ref().or(self.available.as_ref());
        self.github
            .as_ref()
            .filter(|github| local.is_none_or(|local| !newer(&local.version, &github.version)))
            .or(local)
    }
}

impl Target {
    fn settled(&self) -> bool {
        self.skipped.is_some() || terminal(&self.status) || self.status == "unconfirmed"
    }

    fn refresh(
        &mut self,
        command: &Command,
        installed: Option<&str>,
        attempt: Option<&Attempt>,
        now: f64,
    ) {
        let Some(started_at) = command.started_at else {
            return;
        };
        if self.settled() {
            return;
        }
        let expected = if self.local {
            Some(command.request.id.as_str())
        } else {
            self.attempt_id.as_deref()
        };
        if let Some(attempt) = attempt.filter(|attempt| Some(attempt.id.as_str()) == expected) {
            self.status.clone_from(&attempt.status);
            self.reason.clone_from(&attempt.reason);
        } else if installed == Some(command.request.version.as_str())
            && attempt.is_none_or(|attempt| terminal(&attempt.status))
        {
            self.status = "done".into();
            self.reason = None;
        } else if now - started_at >= TIMEOUT {
            self.status = "unconfirmed".into();
            self.reason
                .get_or_insert_with(|| "Check this machine for the update result.".into());
        } else if command.submitted && installed.is_none() {
            self.status = "reconnecting".into();
        }
    }
}

fn targets(
    state: &Status,
    machines: &[machines::Machine],
    peers: &[Peer],
    local_name: String,
) -> Vec<Target> {
    let local = Target {
        id: "local".into(),
        name: local_name,
        current: state.current.version.clone(),
        target: state.available.as_ref().map(|offer| offer.version.clone()),
        source: state.available.as_ref().and_then(|offer| offer.source),
        running: state.running_turns,
        local: true,
        status: "waiting".into(),
        reason: None,
        attempt_id: None,
        skipped: if !state.launcher_available {
            Some("Launcher unavailable".into())
        } else if state
            .last_attempt
            .as_ref()
            .is_some_and(|attempt| !terminal(&attempt.status))
        {
            Some("Already updating".into())
        } else if state.available.is_none() {
            Some("Up to date".into())
        } else {
            None
        },
    };
    let feed = state.feed();
    std::iter::once(local)
        .chain(machines.iter().map(|machine| {
            let peer = peers.iter().find(|peer| peer.machine == machine.id);
            let current = peer
                .and_then(|peer| peer.current.as_ref())
                .map_or(&machine.version, |current| &current.version);
            let skipped = match machine.status.as_str() {
                "unauthorized" => Some("Access revoked"),
                "stale" => Some("Status stale"),
                "offline" => Some("Offline"),
                _ if !machine.allow_updates => Some("Remote updates off"),
                _ if feed.is_some_and(|offer| !newer(&offer.version, current)) => {
                    Some("Up to date")
                }
                _ if peer
                    .and_then(|peer| peer.last_attempt.as_ref())
                    .is_some_and(|attempt| !terminal(&attempt.status)) =>
                {
                    Some("Already updating")
                }
                _ if feed.is_none()
                    || peer.is_none_or(|peer| {
                        (peer.available != true && !peer.available.is_object())
                            || peer.launcher_available != Some(true)
                    }) =>
                {
                    Some(
                        peer.and_then(|peer| peer.reason.as_deref())
                            .unwrap_or("Updates unavailable"),
                    )
                }
                _ => None,
            };
            Target {
                id: machine.id.clone(),
                name: machine.name.clone(),
                current: current.clone(),
                target: feed
                    .filter(|_| skipped.is_none())
                    .map(|offer| offer.version.clone()),
                source: feed.and_then(|offer| offer.source),
                running: peer.and_then(|peer| peer.running_turns).unwrap_or_else(|| {
                    machine
                        .agents
                        .iter()
                        .filter(|agent| agent.status == "running")
                        .count() as u64
                }),
                local: false,
                skipped: skipped.map(str::to_owned),
                status: "waiting".into(),
                attempt_id: None,
                reason: None,
            }
        }))
        .collect()
}

impl Run {
    fn plan(
        rows: Vec<Target>,
        mut create_id: impl FnMut() -> Result<String, String>,
    ) -> Result<Self, String> {
        let eligible = rows.iter().filter(|row| row.skipped.is_none());
        let peers = eligible
            .clone()
            .filter(|row| !row.local)
            .collect::<Vec<_>>();
        let local = eligible.filter(|row| row.local).collect::<Vec<_>>();
        let commands = peers
            .chunks(16)
            .chain((!local.is_empty()).then_some(local.as_slice()))
            .map(|rows| {
                let Some(target) = rows.first() else {
                    return Err("No eligible machines.".into());
                };
                Ok(Command {
                    request: Local {
                        id: create_id()?,
                        version: target.target.clone().ok_or("Update unavailable")?,
                        source: target.source,
                    },
                    machines: rows.iter().map(|row| row.id.clone()).collect(),
                    started_at: None,
                    submitted: false,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let run = Self { rows, commands };
        if !run.valid() {
            return Err("Update plan is invalid. Check machine status and try again.".into());
        }
        Ok(run)
    }

    fn valid(&self) -> bool {
        let mut ids = HashSet::new();
        let mut commands = HashSet::new();
        let mut targets = HashSet::new();
        !self.commands.is_empty()
            && self.rows.iter().all(|row| {
                valid_id(&row.id)
                    && row.local == (row.id == "local")
                    && ids.insert(&row.id)
                    && row.running <= 9_007_199_254_740_991
            })
            && self.commands.iter().enumerate().all(|(index, command)| {
                valid_id(&command.request.id)
                    && commands.insert(&command.request.id)
                    && !command.machines.is_empty()
                    && command.machines.len() <= 16
                    && (!command.machines.iter().any(|id| id == "local")
                        || (command.machines.len() == 1 && index + 1 == self.commands.len()))
                    && command.machines.iter().all(|id| {
                        targets.insert(id)
                            && self.rows.iter().any(|row| {
                                row.id == *id
                                    && row.skipped.is_none()
                                    && row.target.as_ref() == Some(&command.request.version)
                                    && row.source == command.request.source
                            })
                    })
            })
            && targets.len() == self.rows.iter().filter(|row| row.skipped.is_none()).count()
    }

    fn active(&self) -> bool {
        self.rows.iter().any(|row| !row.settled())
    }

    fn next(&self) -> Option<usize> {
        self.commands.iter().position(|command| {
            self.rows
                .iter()
                .any(|row| command.machines.contains(&row.id) && !row.settled())
        })
    }

    fn receipts(&mut self, index: usize, receipts: &[Receipt]) {
        let command = &mut self.commands[index];
        command.submitted = true;
        for row in self
            .rows
            .iter_mut()
            .filter(|row| command.machines.contains(&row.id))
        {
            let receipt = receipts.iter().find(|receipt| receipt.machine == row.id);
            row.attempt_id = receipt.and_then(|receipt| receipt.id.clone());
            row.reason = receipt.and_then(|receipt| receipt.reason.clone());
            row.status = match receipt.map(|receipt| receipt.status.as_str()) {
                Some("failed") => "failed",
                Some("accepted") => "accepted",
                _ => "uncertain",
            }
            .into();
        }
    }
}

async fn submit(command: &Command) -> Result<Vec<Receipt>, String> {
    if command.machines.first().is_some_and(|id| id == "local") {
        let result: Accepted = api::post("/api/updates/apply", &command.request).await?;
        return Ok(vec![Receipt {
            machine: "local".into(),
            status: "accepted".into(),
            reason: None,
            id: Some(result.id),
        }]);
    }
    let mut body = serde_json::to_value(&command.request)
        .map_err(|_| "Update request could not be created.")?;
    body["machines"] = json!(command.machines);
    let result: FanOut = api::post("/api/updates/fan-out", &body).await?;
    Ok(result.machines)
}

#[derive(Clone, PartialEq)]
enum Select {
    All,
    Local(Release),
    Peers(Vec<String>),
}

fn recovery() -> Result<(Option<Run>, Option<Local>), String> {
    let run = restore::<Option<Run>>(RUN_KEY)?.flatten();
    let local = restore::<Option<Local>>(LOCAL_KEY)?.flatten();
    if run.as_ref().is_some_and(|run| !run.valid())
        || local.as_ref().is_some_and(|local| !valid_id(&local.id))
    {
        return Err(
            "Saved update could not be read. Check update history before discarding it.".into(),
        );
    }
    if run.is_some() {
        Ok((run, None))
    } else {
        Ok((None, local))
    }
}

fn clear_local(id: &str) -> Result<(), String> {
    if restore::<Option<Local>>(LOCAL_KEY)?
        .flatten()
        .is_some_and(|local| local.id == id)
    {
        discard(LOCAL_KEY)?;
    }
    Ok(())
}

#[derive(Clone)]
struct SettingsPanel(ViewFn);

pub(crate) fn panel() -> AnyView {
    use_context::<SettingsPanel>().map_or_else(|| ().into_any(), |panel| panel.0.run())
}

pub(crate) fn workspace() -> (impl IntoView, impl IntoView) {
    let machines =
        Query::<machines::Machines>::new(|| machines::PATH.into(), 5000, RwSignal::new(false));
    let environment = Query::<machines::Environment>::new(
        || machines::ENVIRONMENT.into(),
        5000,
        RwSignal::new(false),
    );
    let (restored, legacy, storage_error) = match recovery() {
        Ok((run, legacy)) => (run, legacy, String::new()),
        Err(error) => (None, None, error),
    };
    let opened = RwSignal::new(restored.is_some());
    let run = RwSignal::new(restored);
    let legacy = RwSignal::new(legacy);
    let storage_error = RwSignal::new(storage_error);
    let selection = RwSignal::new(Select::All);
    #[cfg(not(any(feature = "conversation", feature = "workflows", feature = "hub")))]
    let selected = RwSignal::new(Vec::<String>::new());
    let command_issue = RwSignal::new(String::new());
    let flight = RwSignal::new(false);
    let active = move || run.with(|run| run.as_ref().is_some_and(Run::active));
    let urgent = move || {
        active()
            || opened.get()
            || window()
                .location()
                .pathname()
                .is_ok_and(|path| path.starts_with("/settings/"))
    };
    let state = Query::<Status>::paced(
        || "/api/updates".into(),
        1000,
        RwSignal::new(false),
        move || if urgent() { 1000 } else { 15000 },
    );
    let peers_disabled = RwSignal::new(true);
    Effect::new(move |_| {
        peers_disabled.set(
            !urgent()
                && run.with(Option::is_none)
                && machines.value.with(|value| {
                    value
                        .as_ref()
                        .is_none_or(|machines| machines.machines.is_empty())
                }),
        )
    });
    let peers = Query::<Peers>::paced(
        || "/api/updates/machines".into(),
        1000,
        peers_disabled,
        move || if urgent() { 1000 } else { 5000 },
    );
    let is_reconnecting = move || {
        [state.error, machines.error, peers.error, command_issue]
            .iter()
            .any(|error| error.with(|message| reconnecting(message)))
    };
    let issue = move || {
        [state.error, machines.error, peers.error, command_issue]
            .iter()
            .find_map(|signal| {
                signal.with(|message| {
                    (!message.is_empty() && !reconnecting(message)).then(|| message.clone())
                })
            })
            .unwrap_or_default()
    };
    let rows = Memo::new(move |_| {
        let Some(state) = state.value.get() else {
            return Vec::new();
        };
        let machines = machines
            .value
            .get()
            .map(|machines| machines.machines)
            .unwrap_or_default();
        let peers = peers
            .value
            .get()
            .map(|peers| peers.machines)
            .unwrap_or_default();
        let local_name = environment.value.with(|value| {
            value
                .as_ref()
                .map_or_else(|| "This device".into(), |value| value.name.clone())
        });
        targets(&state, &machines, &peers, local_name)
    });
    let plan = Memo::new(move |_| {
        let mut rows = rows.get();
        let Some(state) = state.value.get() else {
            return Vec::new();
        };
        if let Some(legacy) = legacy.get().filter(|_| {
            state.launcher_available
                && state
                    .last_attempt
                    .as_ref()
                    .is_none_or(|attempt| terminal(&attempt.status))
        }) {
            rows.retain(|row| row.local);
            if let Some(row) = rows.first_mut() {
                row.target = Some(legacy.version);
                row.source = legacy.source;
                row.skipped = None;
            }
        } else {
            match selection.get() {
                Select::All => {}
                Select::Peers(ids) => rows.retain(|row| ids.contains(&row.id)),
                Select::Local(offer) => {
                    rows.retain(|row| row.local);
                    if let Some(row) = rows.first_mut().filter(|row| row.skipped.is_none()) {
                        if state.offers().iter().any(|candidate| {
                            candidate.id == offer.id && candidate.source == offer.source
                        }) {
                            row.target = Some(offer.version);
                            row.source = offer.source;
                        } else {
                            row.skipped = Some("Update unavailable".into());
                        }
                    }
                }
            }
        }
        rows
    });
    let planning = move || {
        legacy.with(Option::is_none)
            && (machines.value.with(Option::is_none)
                || (matches!(selection.get(), Select::All | Select::Peers(_))
                    && rows.with(|rows| rows.iter().any(|row| !row.local))
                    && !peers.fresh.get()))
    };
    let save_run = move |next: Run| match save(RUN_KEY, &next) {
        Ok(()) => {
            run.set(Some(next));
            storage_error.set(String::new());
            true
        }
        Err(_) => {
            storage_error.set("Update progress could not be saved. Check browser storage.".into());
            false
        }
    };
    let refresh_state = state.reload;
    let advance = move || {
        if flight.get_untracked()
            || !storage_error.with_untracked(String::is_empty)
            || !state.fresh.get_untracked()
        {
            return;
        }
        let Some(state) = state.value.get_untracked() else {
            return;
        };
        let snapshots = peers
            .value
            .get_untracked()
            .map(|peers| peers.machines)
            .unwrap_or_default();
        let now = js_sys::Date::now();
        if let Some(pending) = legacy.get_untracked().filter(|pending| {
            state
                .last_attempt
                .as_ref()
                .is_some_and(|attempt| attempt.id == pending.id)
        }) {
            if state
                .last_attempt
                .as_ref()
                .is_some_and(|attempt| terminal(&attempt.status))
            {
                match clear_local(&pending.id) {
                    Ok(()) => legacy.set(None),
                    Err(error) => storage_error.set(error),
                }
            } else if let Some(mut local) =
                rows.with_untracked(|rows| rows.iter().find(|row| row.local).cloned())
            {
                local.target = Some(pending.version);
                local.source = pending.source;
                local.skipped = None;
                if let Ok(mut next) = Run::plan(vec![local], || Ok(pending.id.clone())) {
                    next.commands[0].started_at = Some(now);
                    next.commands[0].submitted = true;
                    if save_run(next) {
                        legacy.set(None);
                        opened.set(true);
                    }
                }
            }
        }
        let Some(mut next) = run.get_untracked() else {
            return;
        };
        let Some(index) = next.next() else {
            return;
        };
        let mut command = next.commands[index].clone();
        let local = command.machines.first().is_some_and(|id| id == "local");
        let previous = next.clone();
        for row in next
            .rows
            .iter_mut()
            .filter(|row| command.machines.contains(&row.id))
        {
            if row.local {
                row.refresh(
                    &command,
                    Some(&state.current.version),
                    state.last_attempt.as_ref(),
                    now,
                );
            } else {
                let peer = snapshots.iter().find(|peer| peer.machine == row.id);
                row.refresh(
                    &command,
                    peer.and_then(|peer| peer.current.as_ref().map(|value| value.version.as_str())),
                    peer.and_then(|peer| peer.last_attempt.as_ref()),
                    now,
                );
            }
        }
        if next != previous && !save_run(next.clone()) {
            return;
        }
        if command.submitted || next.next() != Some(index) {
            return;
        }
        if local
            && state
                .last_attempt
                .as_ref()
                .is_some_and(|attempt| attempt.id == command.request.id)
        {
            next.commands[index].submitted = true;
            save_run(next);
            command_issue.set(String::new());
            return;
        }
        if command.started_at.is_none() {
            command.started_at = Some(now);
            next.commands[index] = command.clone();
            for row in next
                .rows
                .iter_mut()
                .filter(|row| command.machines.contains(&row.id))
            {
                row.status = "submitting".into();
            }
            if !save_run(next.clone()) {
                return;
            }
        }
        if local && save(LOCAL_KEY, &command.request).is_err() {
            storage_error.set("Update request could not be saved. Check browser storage.".into());
            return;
        }
        flight.set(true);
        leptos::task::spawn_local(async move {
            let result = submit(&command).await;
            if flight.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(receipts) => {
                    next.receipts(index, &receipts);
                    save_run(next);
                    command_issue.set(String::new());
                }
                Err(error)
                    if reconnecting(&error)
                        || error == "Minarbot returned an invalid response. Try again." =>
                {
                    command_issue.set("Minarbot connection lost. Checking update status.".into())
                }
                Err(error) => {
                    let receipts = command
                        .machines
                        .iter()
                        .map(|id| Receipt {
                            machine: id.clone(),
                            status: "failed".into(),
                            reason: Some(error.clone()),
                            id: None,
                        })
                        .collect::<Vec<_>>();
                    next.receipts(index, &receipts);
                    save_run(next);
                    command_issue.set(String::new());
                }
            }
            flight.set(false);
            refresh_state.update(|value| *value = value.wrapping_add(1));
            peers.retry();
        });
    };
    poll(1000, advance);
    let begin = move || {
        if run.with_untracked(Option::is_some)
            || !state.fresh.get_untracked()
            || !storage_error.with_untracked(String::is_empty)
            || untrack(planning)
            || untrack(is_reconnecting)
            || !untrack(issue).is_empty()
        {
            return;
        }
        match Run::plan(plan.get_untracked(), uuid) {
            Ok(mut next) => {
                if let Some(legacy) = legacy.get_untracked() {
                    let Some(command) = next
                        .commands
                        .iter_mut()
                        .find(|command| command.machines.iter().any(|id| id == "local"))
                    else {
                        return;
                    };
                    if command.request.version != legacy.version
                        || command.request.source != legacy.source
                    {
                        return;
                    }
                    command.request.id = legacy.id;
                }
                if save_run(next) {
                    legacy.set(None);
                    advance();
                }
            }
            Err(error) => storage_error.set(error),
        }
    };
    let close = move || {
        if let Some(previous) = run.get_untracked().filter(|run| !run.active()) {
            let result = (|| {
                if let Some(command) = previous
                    .commands
                    .iter()
                    .find(|command| command.machines.iter().any(|id| id == "local"))
                {
                    if previous
                        .rows
                        .iter()
                        .any(|row| row.local && row.status != "unconfirmed")
                    {
                        clear_local(&command.request.id)?;
                    }
                }
                discard(RUN_KEY)?;
                restore::<Option<Local>>(LOCAL_KEY).map(Option::flatten)
            })();
            match result {
                Ok(pending) => {
                    run.set(None);
                    legacy.set(pending);
                }
                Err(error) => {
                    storage_error.set(error);
                    return;
                }
            }
        }
        opened.set(false);
    };
    let show = move |choice| {
        selection.set(choice);
        opened.set(true);
        state.retry();
    };
    #[cfg(not(any(feature = "conversation", feature = "workflows", feature = "hub")))]
    let chosen = Memo::new(move |_| {
        rows.with(|rows| {
            rows.iter()
                .filter(|row| {
                    !row.local
                        && row.skipped.is_none()
                        && selected.with(|ids| ids.contains(&row.id))
                })
                .map(|row| row.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let row_display = Memo::new(move |_| run.get().map_or_else(|| plan.get(), |run| run.rows));
    let count =
        move || row_display.with(|rows| rows.iter().filter(|row| row.skipped.is_none()).count());
    let download = move |id: &str| {
        if id == "local" {
            state.value.with(|state| {
                state
                    .as_ref()
                    .and_then(|state| state.last_attempt.as_ref())
                    .filter(|attempt| attempt.status == "downloading")
                    .map(|attempt| attempt.download.clone())
            })
        } else {
            peers.value.with(|peers| {
                peers
                    .as_ref()
                    .and_then(|peers| peers.machines.iter().find(|peer| peer.machine == id))
                    .and_then(|peer| peer.fan_out_attempt.as_ref())
                    .filter(|transfer| transfer.state == "transferring")
                    .map(|transfer| transfer.download.clone())
            })
        }
    };
    let version_text = move || {
        row_display.with(|rows| {
            rows.iter()
                .find(|row| row.skipped.is_none())
                .and_then(|row| {
                    row.target.as_ref().map(|version| {
                        row.source.map_or_else(
                            || format!("Minarbot {version}"),
                            |source| format!("Minarbot {version} · {}", source.label()),
                        )
                    })
                })
                .unwrap_or_default()
        })
    };
    let title = move || {
        let count = count();
        let word = if count == 1 { "machine" } else { "machines" };
        if run.with(Option::is_some) {
            if active() {
                format!("Updating {count} {word}")
            } else if row_display.with(|rows| {
                rows.iter()
                    .any(|row| row.skipped.is_none() && row.status != "done")
            }) {
                "Update results".into()
            } else {
                "Update complete".into()
            }
        } else if !storage_error.with(String::is_empty) {
            "Update recovery".into()
        } else if planning() || count == 0 {
            "Update machines".into()
        } else {
            format!("Update {count} {word}?")
        }
    };

    let ready = move || rows.with(|rows| rows.iter().any(|row| row.skipped.is_none()));
    let capsule = Memo::new(move |_| {
        let mut downloads = state.value.with(|status| {
            status
                .as_ref()
                .and_then(|status| status.last_attempt.as_ref())
                .filter(|attempt| attempt.status == "downloading")
                .map(|attempt| attempt.download.clone())
                .into_iter()
                .collect::<Vec<_>>()
        });
        peers.value.with(|status| {
            downloads.extend(
                status
                    .iter()
                    .flat_map(|status| status.machines.iter())
                    .filter_map(|peer| peer.fan_out_attempt.as_ref())
                    .filter(|attempt| attempt.state == "transferring")
                    .map(|attempt| attempt.download.clone()),
            );
        });
        let source = if let Some(first) = downloads.first() {
            downloads
                .iter()
                .all(|entry| entry.source == first.source)
                .then_some(first.source)
                .flatten()
        } else if run.with(Option::is_none) {
            rows.with(|rows| {
                rows.iter()
                    .find(|row| row.skipped.is_none())
                    .and_then(|row| row.source)
            })
        } else {
            None
        };
        let download = (!downloads.is_empty()).then(|| Download {
            source,
            bytes_transferred: Some(
                downloads
                    .iter()
                    .filter_map(|download| download.bytes_transferred)
                    .fold(0_u64, u64::saturating_add),
            ),
            total_bytes: Some(
                downloads
                    .iter()
                    .filter_map(|download| download.total_bytes)
                    .fold(0_u64, u64::saturating_add),
            ),
        });
        let (label, attention) = if !storage_error.with(String::is_empty) {
            ("Update recovery".into(), true)
        } else if is_reconnecting() {
            ("Reconnecting".into(), false)
        } else if let Some(download) = &download {
            (download.label(), false)
        } else if let Some(run) = run.get() {
            let machines = run.rows.iter().filter(|row| row.skipped.is_none());
            let failed = machines
                .clone()
                .filter(|row| matches!(row.status.as_str(), "failed" | "rolled-back"))
                .count();
            let unconfirmed = machines
                .clone()
                .filter(|row| row.status == "unconfirmed")
                .count();
            if !run.active() && failed > 0 {
                (
                    format!(
                        "{failed} update{} failed",
                        if failed == 1 { "" } else { "s" }
                    ),
                    true,
                )
            } else if !run.active() && unconfirmed > 0 {
                (
                    format!(
                        "{unconfirmed} update{} unconfirmed",
                        if unconfirmed == 1 { "" } else { "s" }
                    ),
                    true,
                )
            } else {
                let done = machines.clone().filter(|row| row.status == "done").count();
                (format!("{done} of {} updated", machines.count()), false)
            }
        } else {
            ("Update available".into(), false)
        };
        (
            ready() || run.with(Option::is_some) || download.is_some() || attention,
            label,
            source,
            attention,
        )
    });
    let capsule_icons = icons();
    let capsule_view = view! {
        <div class="sidebar-update" hidden=move || !capsule.with(|(visible, _, _, _)| *visible)>
          <button type="button" class="k-btn k-btn--md k-btn--secondary sidebar-update__capsule" aria-haspopup="dialog" on:click=move |_| show(Select::All)>
            <span class="appearance-icon k-btn__icon" aria-hidden="true" inner_html=move || {
              if capsule.with(|(_, _, _, attention)| *attention) { capsule_icons.compact_alert.clone() }
              else if run.with(Option::is_some) && !active() { capsule_icons.check.clone() }
              else { capsule_icons.update.clone() }
            }></span>
            <span class="sidebar-update__text">
              <span>{move || capsule.with(|(_, label, _, _)| label.clone())}</span>
              {move || capsule.with(|(_, _, source, _)| *source).filter(|_| storage_error.with(String::is_empty)).map(|source| view! { <small>{source.label()}</small> })}
            </span>
          </button>
        </div>
    };
    // The Updates settings route mounts in the appearance bundle.
    #[cfg(not(any(feature = "conversation", feature = "workflows", feature = "hub")))]
    let settings = ViewFn::from(move || {
        view! { <div class="k-page bs-root updates-page set4-page">
            <Header title="Updates" actions=content(move || view! {
              <Show when=move || run.with(Option::is_some) || !storage_error.with(String::is_empty)>
                <Button label=Signal::derive(move || if storage_error.with(String::is_empty) { "Update progress".to_owned() } else { "Update recovery".into() })
                  on_click=move || show(Select::All) />
              </Show>
            }) />
            <Show when=move || !issue().is_empty()><Alert tone="error">{issue}</Alert></Show>
            <Show when=is_reconnecting><Alert title=text("Reconnecting") /></Show>
            <Show when=move || state.value.with(Option::is_some) fallback=move || skeleton(3, "Loading updates...")>
              {move || state.value.get().map(|status| {
                let offers = status.offers();
                let multiple = offers.len() > 1;
                let disabled = Signal::derive(move || active() || run.with(Option::is_some) || state.value.with(|value| value.as_ref().and_then(|value| value.last_attempt.as_ref()).is_some_and(|attempt| !terminal(&attempt.status))));
                let running = status.running_turns;
                let commit = if status.current.id == "development" { "Development".into() } else { status.current.commit.chars().take(8).collect::<String>() };
                let no_offer = offers.is_empty();
                view! {
                  {status.github_error.clone().map(|error| view! { <Alert tone="warn">{error}</Alert> })}
                  <Section title="This device"><Card>
                    <Row leading=icons().monitor title=text("Current version") meta=content(move || commit.into_view())
                      trailing=content(move || view! { {badge("neutral", status.current.version)} }) />
                    {status.previous.filter(|_| status.launcher_available).map(|previous| view! {
                      <Row title=text("Previous version") trailing=content(move || view! { {badge("neutral", previous.version)} }) />
                    })}
                    {status.launcher_available.then(|| offers.into_iter().map(|release| {
                      let selected_offer = release.clone();
                      let source = release.source.map_or("Local feed", Source::label);
                      let created = String::from(date(release.created).to_locale_date_string("default", &JsValue::UNDEFINED));
                      view! {
                        <Row leading=icons().download title=text(format!("Minarbot {}", release.version))
                          meta=content(move || format!("{source} · Published {created}").into_view())
                          trailing=content(|| view! { {badge("brand", "Update available")} }) />
                        <div class="updates-release"><p class="updates-notes">{release.notes}</p><div class="updates-actions">
                          <span class="k-field__hint">{format!("{running} running {}", if running == 1 { "turn" } else { "turns" })}</span>
                          <button type="button" class="k-btn k-btn--primary k-btn--md" disabled=disabled
                            aria-label=move || (multiple && legacy.with(Option::is_none)).then(|| format!("Update from {source}"))
                            on:click=move |_| show(Select::Local(selected_offer.clone()))>
                            {move || if legacy.with(Option::is_some) { "Retry update" } else { "Update now" }}
                          </button>
                        </div></div>
                      }
                    }).collect_view())}
                    {(status.launcher_available && no_offer).then(|| view! {
                      <Row title=text(if status.github_error.is_some() { "No updates available" } else { "Up to date" }) />
                    })}
                  </Card></Section>
                  {status.last_attempt.map(|attempt| {
                    let status = attempt.status.clone();
                    let is_downloading = status == "downloading";
                    let title = if is_downloading { attempt.download.label() } else { label(&status).to_owned() };
                    let source = attempt.download.source.map_or_else(String::new, |source| format!(" · {}", source.label()));
                    let reason = attempt.reason.map_or_else(String::new, |reason| format!(" - {reason}"));
                    view! { <Section title="Last update"><Alert tone=if matches!(status.as_str(), "failed" | "rolled-back") { "warn" } else { "info" } title=text(title)>
                      <span>{format!("Version {}{source}{reason}", attempt.version)}</span>
                      {attempt.download.total_bytes.filter(|total| is_downloading && *total > 0).map(|total| view! {
                        <progress class="updates-download" aria-label="Download progress" max=total value=attempt.download.bytes_transferred.unwrap_or_default()></progress>
                      })}
                    </Alert></Section> }
                  })}
                }
              })}
              <Section title="Paired machines" actions=content(move || view! {
                <Show when=move || !chosen.with(Vec::is_empty)>
                  <Button label=Signal::derive(move || format!("Update selected ({})", chosen.with(Vec::len)))
                    disabled=Signal::derive(move || run.with(Option::is_some)) on_click=move || show(Select::Peers(chosen.get_untracked())) />
                </Show>
              })>
                {move || {
                  let rows = rows.get().into_iter().filter(|row| !row.local).collect::<Vec<_>>();
                  if rows.is_empty() { return empty("No paired machines", String::new()).into_any(); }
                  view! { <Card>{rows.into_iter().map(|row| {
                    let id = StoredValue::new(row.id);
                    let name = row.name.clone();
                    let can_update = row.skipped.is_none();
                    let meta = row.skipped.unwrap_or_else(|| row.source.map_or_else(|| "Ready to update".into(), |source| format!("Ready to update · {}", source.label())));
                    view! { <Row title=text(row.name)
                      meta=content(move || view! {
                        {move || id.with_value(|id| download(id)).map_or_else(|| meta.clone(), |download|
                          download.source.map_or_else(|| download.label(), |source| format!("{} · {}", source.label(), download.label())))}
                        <span>{format!("{} running turns", row.running)}</span>
                      })
                      trailing=content(move || view! {
                        {badge("neutral", row.current)}
                        {can_update.then(|| view! { <label class="k-inline"><span>{format!("Also update {name}")}</span>
                          <button class="k-switch" type="button" role="switch" aria-label=format!("Also update {name}")
                            aria-checked=move || selected.with(|ids| id.with_value(|id| ids.contains(id))).to_string()
                            data-state=move || if selected.with(|ids| id.with_value(|id| ids.contains(id))) { "checked" } else { "unchecked" }
                            on:click=move |_| selected.update(|items| id.with_value(|id| {
                              if let Some(index) = items.iter().position(|item| item == id) { items.remove(index); } else { items.push(id.clone()); }
                            }))><span class="k-switch__thumb" data-state=move || if selected.with(|ids| id.with_value(|id| ids.contains(id))) { "checked" } else { "unchecked" }></span></button>
                        </label> })}
                      }) /> }
                  }).collect_view()}</Card> }.into_any()
                }}
              </Section>
            </Show>
          </div>
        }
    });
    #[cfg(not(any(feature = "conversation", feature = "workflows", feature = "hub")))]
    provide_context(SettingsPanel(settings));
    let dialog = view! {
        <Dialog id="updates-plan" title=Signal::derive(title) description=Signal::derive(version_text) open=opened on_close=close size="lg"
          footer=content(move || view! {
            <Show when=move || run.with(Option::is_none) && !planning() && (count() > 0)>
              <span class="k-dialog__foot-note updates-warning">{move || {
                let running = row_display.with(|rows| rows.iter().filter(|row| row.skipped.is_none()).map(|row| row.running).sum::<u64>());
                format!("{running} running {} will be interrupted and can resume after the restart.", if running == 1 { "turn" } else { "turns" })
              }}</span>
            </Show>
            <Button label=Signal::derive(move || if run.with(Option::is_some) { "Close".to_owned() } else { "Cancel".into() }) on_click=close />
            <Show when=move || run.with(Option::is_none)>
              <Button label=text("Update") variant="primary" on_click=begin disabled=Signal::derive(move ||
                count() == 0 || planning() || is_reconnecting() || !issue().is_empty() || !storage_error.with(String::is_empty) || !state.fresh.get()) />
            </Show>
          })>
          <div class="k-stack">
            <Show when=move || !storage_error.with(String::is_empty)>
              <Alert tone="error" actions=content(move || view! {
                <Show when=move || run.with(Option::is_none)>
                  <Button label=text("Discard saved update") size="sm" on_click=move || match discard(RUN_KEY).and_then(|()| discard(LOCAL_KEY)) {
                    Ok(()) => { legacy.set(None); storage_error.set(String::new()); },
                    Err(error) => storage_error.set(error),
                  } />
                </Show>
              })>{storage_error}</Alert>
            </Show>
            <Show when=is_reconnecting><p role="status" class="k-field__hint">"Reconnecting…"</p></Show>
            <Show when=move || !issue().is_empty()><Alert tone="error">{issue}</Alert></Show>
            <Show when=move || run.with(Option::is_some)>
              <p class="sr-only" role="status">{move || row_display.with(|rows| {
                let selected = rows.iter().filter(|row| row.skipped.is_none());
                let done = selected.clone().filter(|row| row.status == "done").count();
                format!("{done} of {} updated", selected.count())
              })}</p>
            </Show>
            <Show when=move || (run.with(Option::is_some) || !planning()) && !row_display.with(Vec::is_empty)
              fallback=move || view! { <Show when=move || planning() && storage_error.with(String::is_empty)>{skeleton(3, "Loading update targets...")}</Show> }>
              <div class="k-table-wrap"><table class="k-table updates-table"><thead><tr><th scope="col">"Machine"</th><th scope="col">"Minarbot"</th><th scope="col">"Running turns"</th></tr></thead>
                <tbody>{move || row_display.get().into_iter().map(|row| {
                  let id = StoredValue::new(row.id);
                  let status = row.status;
                  let tone = match status.as_str() { "done" => "success", "failed" | "rolled-back" | "unconfirmed" => "warn", _ => "neutral" };
                  view! { <tr>
                    <th scope="row"><span>{row.name}</span>{row.local.then(|| view! { <small>"Connected server"</small> })}</th>
                    <td><span class="updates-version">{row.current}{row.target.map(|target| view! { {icon(icons().arrow)}<span class="sr-only">"to"</span>{target} })}</span>
                      {if let Some(skipped) = row.skipped { view! { <small>{skipped}</small> }.into_any() } else { run.with(Option::is_some).then(|| view! {
                        <div class="updates-result">
                          {badge(tone, move || id.with_value(|id| download(id)).map_or_else(|| label(&status).into(), |download| download.label()))}
                          {move || id.with_value(|id| download(id)).and_then(|download| download.source).map(|source| view! { <small>{source.label()}</small> })}
                          {row.reason.map(|reason| view! { <small>{reason}</small> })}
                        </div>
                      }).into_any() }}
                    </td><td>{row.running}</td>
                  </tr> }
                }).collect_view()}</tbody>
              </table></div>
            </Show>
            <Show when=move || run.with(Option::is_none)>
              {move || {
                let release = row_display.with(|rows| rows.iter().find(|row| row.skipped.is_none()).map(|row| (row.target.clone(), row.source)));
                release.and_then(|(version, source)| state.value.with(|state| state.as_ref().and_then(|state|
                  [&state.available, &state.github, &state.feed].into_iter().flatten().find(|release| Some(&release.version) == version.as_ref() && release.source == source).map(|release| release.notes.clone())
                ))).filter(|notes| !notes.trim().is_empty()).map(|notes| view! {
                  <Disclosure title="Release notes"><p class="updates-notes">{notes}</p></Disclosure>
                })
              }}
            </Show>
          </div>
        </Dialog>
    };
    (capsule_view, dialog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_out_precedes_local_and_recovers_progress() -> Result<(), String> {
        let rows = (0..17)
            .map(|index| format!("peer-{index}"))
            .chain(["local".into()])
            .map(|id| Target {
                local: id == "local",
                name: id.clone(),
                id,
                current: "0.15.9".into(),
                target: Some("0.16.0".into()),
                source: Some(Source::Github),
                running: 0,
                skipped: None,
                status: "waiting".into(),
                reason: None,
                attempt_id: None,
            })
            .collect();
        let mut id = 0;
        let run = Run::plan(rows, || {
            id += 1;
            Ok(format!("request-{id}"))
        })?;
        assert_eq!(
            run.commands
                .iter()
                .map(|command| command.machines.len())
                .collect::<Vec<_>>(),
            [16, 1, 1]
        );
        assert_eq!(run.commands[2].machines, ["local"]);
        let raw = serde_json::to_string(&run).map_err(|error| error.to_string())?;
        let mut restored: Run = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        assert!(restored.valid());
        restored.commands[0].started_at = Some(1.0);
        restored.rows[0].refresh(&restored.commands[0], Some("0.16.0"), None, 2.0);
        assert_eq!(restored.rows[0].status, "done");
        assert!(newer("0.16.0", "0.16.0-beta"));
        assert!(!newer("0.16.0-beta2", "0.16.0-beta"));
        Ok(())
    }
}
