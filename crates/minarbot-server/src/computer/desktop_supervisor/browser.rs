use super::cdp::{Connection, Error, Result};
use crate::computer::{
    actions::http_url,
    browser_actions::{key_code, Action, Reply, Request, TabAction},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    time::Duration,
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

const MAX_TABS: usize = 24;
const STALE: Error = Error::Rejected("Stale browser element ref. Take a fresh browser snapshot.");
const UNAVAILABLE: Error =
    Error::Rejected("Browser element is unavailable. Refresh the snapshot or use Computer pixels.");
const DOM: &str = include_str!("browser_dom.js");

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    target_id: String,
    browser_context_id: Option<String>,
    url: String,
    title: String,
}

struct Tab {
    id: String,
    session: String,
    url: String,
    title: String,
}

#[derive(Deserialize, Serialize)]
struct SavedTabs {
    active: String,
    tabs: Vec<SavedTab>,
}

#[derive(Deserialize, Serialize)]
struct SavedTab {
    id: String,
    url: String,
}

struct Page {
    target: String,
    session: String,
    context: Value,
    stamp: Value,
    document: Value,
    navigation: u64,
    visible: bool,
    focused: bool,
    allowed: bool,
}

struct Snapshot {
    target: String,
    document: Value,
    stamp: Value,
    navigation: u64,
    // Keys include a fresh generation nonce. Restart and new snapshots retire every old key.
    elements: HashMap<String, u64>,
}

pub(super) struct Browser {
    cdp: Connection,
    profile: String,
    file: PathBuf,
    saved: Vec<u8>,
    tabs: HashMap<String, Tab>,
    active: Option<String>,
    snapshot: Option<Snapshot>,
    epoch: u64,
    hovered: Option<(String, Value)>,
    sensitive: bool,
}

impl Browser {
    pub async fn start(mut cdp: Connection, file: PathBuf) -> Result<Self> {
        let mut initial = targets(&mut cdp).await?;
        if initial.len() != 1 || initial[0].url != "about:blank" {
            return Err(Error::Rejected(
                "Chrome did not launch its dedicated profile.",
            ));
        }
        let first = initial.pop().ok_or(Error::Command)?;
        let profile = first.browser_context_id.clone().ok_or(Error::Command)?;
        let mut browser = Self {
            cdp,
            profile,
            file,
            saved: vec![],
            tabs: HashMap::new(),
            active: None,
            snapshot: None,
            epoch: 0,
            hovered: None,
            sensitive: false,
        };
        let saved = browser.read_tabs()?;
        let target = first.target_id.clone();
        if let Some(saved) = saved {
            let mut active = None;
            let mut first_restored = None;
            for tab in saved.tabs {
                let target = browser.open(&tab.url, &tab.id).await?;
                if tab.id == saved.active {
                    active = Some(target.clone());
                }
                first_restored.get_or_insert(target);
            }
            browser
                .cdp
                .call(None, "Target.closeTarget", json!({"targetId":target}))
                .await?;
            browser
                .activate(&active.or(first_restored).ok_or(Error::Command)?)
                .await?;
        } else {
            browser
                .attach(first.target_id, &tab_id(), "about:blank", &first.title)
                .await?;
            browser.active = Some(target);
        }
        browser.save_tabs()?;
        Ok(browser)
    }

    fn read_tabs(&mut self) -> Result<Option<SavedTabs>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(&self.file)
        {
            Ok(file) if file.metadata().is_ok_and(|m| m.is_file()) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            _ => return Err(Error::Command),
        };
        let mut bytes = Vec::new();
        file.take(256 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Command)?;
        let saved: SavedTabs = serde_json::from_slice(&bytes).map_err(|_| Error::Command)?;
        let mut ids = HashSet::new();
        if saved.tabs.is_empty()
            || saved.tabs.len() > MAX_TABS
            || saved.tabs.iter().any(|tab| {
                !tab.id.starts_with('t')
                    || Uuid::parse_str(&tab.id[1..]).is_err()
                    || !ids.insert(&tab.id)
                    || tab.url != "about:blank" && http_url(&tab.url).is_err()
            })
            || !saved.active.is_empty() && !ids.contains(&saved.active)
        {
            return Err(Error::Command);
        }
        self.saved = bytes;
        Ok(Some(saved))
    }

    fn save_tabs(&mut self) -> Result<()> {
        if self.sensitive || self.tabs.is_empty() {
            return Ok(());
        }
        let mut tabs = self
            .tabs
            .values()
            .map(|tab| SavedTab {
                id: tab.id.clone(),
                url: http_url(&tab.url).unwrap_or_else(|_| "about:blank".into()),
            })
            .collect::<Vec<_>>();
        tabs.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        let bytes = serde_json::to_vec(&SavedTabs {
            active: self
                .active
                .as_ref()
                .and_then(|id| self.tabs.get(id))
                .map(|tab| tab.id.clone())
                .unwrap_or_default(),
            tabs,
        })
        .map_err(|_| Error::Command)?;
        if bytes != self.saved {
            let temp = self
                .file
                .with_extension(Uuid::new_v4().simple().to_string());
            let result = (|| -> std::io::Result<()> {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temp)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                fs::rename(&temp, &self.file)?;
                if let Some(parent) = self.file.parent() {
                    File::open(parent)?.sync_all()?;
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(temp);
                return Err(Error::Command);
            }
            self.saved = bytes;
        }
        Ok(())
    }

    async fn attach(&mut self, target: String, id: &str, url: &str, title: &str) -> Result<()> {
        let reply = self
            .cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
            )
            .await?;
        let session = reply["sessionId"]
            .as_str()
            .ok_or(Error::Command)?
            .to_owned();
        self.cdp.watch(&session)?;
        self.tabs.insert(
            target,
            Tab {
                id: id.into(),
                session: session.clone(),
                url: url.into(),
                title: title.into(),
            },
        );
        for method in ["Page.enable", "Network.enable"] {
            self.cdp.call(Some(&session), method, json!({})).await?;
        }
        Ok(())
    }

    async fn refresh(&mut self) -> Result<()> {
        let targets = targets(&mut self.cdp).await?;
        let targets = targets
            .into_iter()
            .filter(|t| t.browser_context_id.as_ref() == Some(&self.profile))
            .collect::<Vec<_>>();
        if targets.len() > MAX_TABS {
            return Err(Error::Rejected(
                "Too many Chrome tabs. Close tabs in Computer.",
            ));
        }
        let live: HashSet<_> = targets.iter().map(|t| t.target_id.as_str()).collect();
        let dead = self
            .tabs
            .keys()
            .filter(|target| !live.contains(target.as_str()))
            .filter_map(|target| {
                self.tabs
                    .get(target)
                    .map(|tab| (target.clone(), tab.session.clone()))
            })
            .collect::<Vec<_>>();
        self.tabs.retain(|target, _| live.contains(target.as_str()));
        for (_, session) in dead {
            self.cdp.forget(&session);
        }
        for Target {
            target_id,
            url,
            title,
            ..
        } in targets
        {
            if let Some(tab) = self.tabs.get_mut(&target_id) {
                tab.url = url;
                tab.title = title;
            } else {
                self.attach(target_id, &tab_id(), &url, &title).await?;
            }
        }
        Ok(())
    }

    pub async fn checkpoint(&mut self) -> Result<()> {
        self.refresh().await?;
        let _ = self.active_page().await;
        self.save_tabs()
    }

    pub async fn close(&mut self) {
        let _ = timeout(Duration::from_millis(600), self.checkpoint()).await;
        let _ = timeout(
            Duration::from_millis(300),
            self.cdp.call(None, "Browser.close", json!({})),
        )
        .await;
    }

    async fn open(&mut self, url: &str, id: &str) -> Result<String> {
        let result = self
            .cdp
            .call(
                None,
                "Target.createTarget",
                json!({"url":url,"browserContextId":self.profile}),
            )
            .await?;
        let target = result["targetId"]
            .as_str()
            .ok_or(Error::Command)?
            .to_owned();
        self.attach(target.clone(), id, url, "").await?;
        Ok(target)
    }

    async fn activate(&mut self, target: &str) -> Result<()> {
        self.clear_hover().await;
        self.cdp
            .call(None, "Target.activateTarget", json!({"targetId":target}))
            .await?;
        self.active = Some(target.into());
        self.snapshot = None;
        Ok(())
    }

    async fn active_page(&mut self) -> Result<Page> {
        let mut ids = self.tabs.keys().cloned().collect::<Vec<_>>();
        ids.sort_by_key(|id| self.active.as_ref() != Some(id));
        let mut visible = None;
        let mut ambiguous = false;
        for id in ids {
            let Some(tab) = self.tabs.get(&id) else {
                continue;
            };
            let session = tab.session.clone();
            let page = match self.page(id, session).await {
                Err(Error::Command) => continue,
                other => other?,
            };
            if page.visible && page.focused {
                return Ok(self.set_active(page));
            }
            if page.visible {
                ambiguous |= visible.replace(page).is_some();
            }
        }
        if let Some(page) = visible.filter(|_| !ambiguous) {
            return Ok(self.set_active(page));
        }
        Err(Error::Rejected(
            "No unambiguous active Chrome tab. Switch to a tab or use Computer pixels.",
        ))
    }

    fn set_active(&mut self, page: Page) -> Page {
        if self.active.as_ref() != Some(&page.target) {
            self.snapshot = None;
            self.active = Some(page.target.clone());
        }
        page
    }

    async fn page(&mut self, target: String, session: String) -> Result<Page> {
        let tree = self
            .cdp
            .call(Some(&session), "Page.getFrameTree", json!({}))
            .await?;
        let frame = &tree["frameTree"]["frame"];
        let frame_id = frame["id"].as_str().ok_or(Error::Command)?;
        self.cdp
            .events(&session, |events| frame_id.clone_into(&mut events.frame))?;
        let world = self
            .cdp
            .call(
                Some(&session),
                "Page.createIsolatedWorld",
                json!({"frameId":frame_id,"worldName":"Minarbot"}),
            )
            .await?;
        let context = world["executionContextId"].clone();
        if !context.is_i64() {
            return Err(Error::Command);
        }
        let state = self
            .dom(
                &session,
                json!({"executionContextId":context}),
                [
                    "state".into(),
                    Value::Null,
                    Value::Null,
                    false.into(),
                    false.into(),
                ],
            )
            .await?;
        Ok(Page {
            target,
            context,
            session: session.clone(),
            stamp: state["stamp"].clone(),
            document: frame["loaderId"].clone(),
            navigation: self.cdp.events(&session, |events| events.navigation)?,
            visible: state["visible"] == true,
            focused: state["focused"] == true,
            allowed: state["validPage"] == true,
        })
    }

    async fn dom(
        &mut self,
        session: &str,
        mut params: Value,
        arguments: [Value; 5],
    ) -> Result<Value> {
        params["functionDeclaration"] = DOM.into();
        params["userGesture"] =
            matches!(arguments[0].as_str(), Some("click" | "type" | "select")).into();
        params["arguments"] = json!(arguments.map(|value| json!({"value":value})));
        params["returnByValue"] = true.into();
        let result = self
            .cdp
            .call(Some(session), "Runtime.callFunctionOn", params)
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err(Error::Command);
        }
        let value = result["result"]["value"].clone();
        if value["stale"] == true {
            Err(STALE)
        } else if value["unavailable"] == true {
            Err(UNAVAILABLE)
        } else if value.is_object() {
            Ok(value)
        } else {
            Err(Error::Command)
        }
    }

    fn backend(&self, page: &Page, reference: &str) -> Result<u64> {
        self.snapshot
            .as_ref()
            .filter(|s| {
                s.target == page.target
                    && s.document == page.document
                    && s.navigation == page.navigation
                    && s.stamp == page.stamp
            })
            .and_then(|s| s.elements.get(reference).copied())
            .ok_or(STALE)
    }

    async fn element(&mut self, page: &Page, reference: &str) -> Result<Value> {
        let backend = self.backend(page, reference)?;
        let node = self.cdp.call(Some(&page.session), "DOM.resolveNode", json!({
            "backendNodeId":backend,"executionContextId":page.context.clone(),"objectGroup":"minarbot"
        })).await.map_err(|e| match e { Error::Command => STALE, other => other })?;
        let id = node["object"]["objectId"].as_str().ok_or(STALE)?;
        Ok(json!({"objectId":id}))
    }

    async fn on_element(
        &mut self,
        page: &Page,
        reference: &str,
        op: &str,
        value: Value,
        submit: bool,
        redacted: bool,
    ) -> Result<Value> {
        let params = self.element(page, reference).await?;
        let result = self
            .dom(
                &page.session,
                params,
                [
                    op.into(),
                    page.stamp.clone(),
                    value,
                    submit.into(),
                    redacted.into(),
                ],
            )
            .await;
        // Releasing the handle must never turn a confirmed input into a retry.
        let _ = self
            .cdp
            .call(
                Some(&page.session),
                "Runtime.releaseObjectGroup",
                json!({"objectGroup":"minarbot"}),
            )
            .await;
        result
    }

    async fn type_into_ref(
        &mut self,
        page: &Page,
        reference: &str,
        text: String,
        redacted: bool,
        submit: bool,
    ) -> Result<()> {
        self.on_element(page, reference, "type", text.into(), submit, redacted)
            .await?;
        Ok(())
    }

    async fn page_dom(&mut self, page: &Page, op: &str, value: Value) -> Result<Value> {
        self.dom(
            &page.session,
            json!({"executionContextId":page.context.clone()}),
            [
                op.into(),
                page.stamp.clone(),
                value,
                false.into(),
                false.into(),
            ],
        )
        .await
    }

    fn invalidate(&mut self) {
        self.snapshot = None;
    }

    async fn refresh_epoch(&mut self, epoch: u64) -> Result<()> {
        if self.epoch != epoch {
            self.clear_hover().await;
            self.invalidate();
            self.epoch = epoch;
        }
        self.refresh().await.map_err(preflight)
    }

    pub async fn focused(
        &mut self,
        origin: &str,
        target: &str,
        value: Option<&str>,
    ) -> Result<bool> {
        let sensitive = self.sensitive;
        self.sensitive |= value.is_some();
        let result = self.cdp.focused(origin, target, value).await;
        if matches!(result, Ok(false)) {
            self.sensitive = sensitive;
        }
        result
    }

    pub async fn secure_type(
        &mut self,
        origin: &str,
        reference: &str,
        text: &str,
        epoch: u64,
    ) -> Result<bool> {
        self.refresh_epoch(epoch).await?;
        let page = match self.active_page().await {
            Ok(page) => page,
            Err(Error::Rejected(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        if !page.focused
            || page.stamp[0]
                .as_str()
                .and_then(|href| url::Url::parse(href).ok())
                .is_none_or(|url| url.origin().ascii_serialization() != origin)
        {
            return Ok(false);
        }
        // The element action rechecks the live document and node in the isolated world.
        let sensitive = self.sensitive;
        self.sensitive = true;
        match self
            .type_into_ref(&page, reference, text.to_owned(), true, false)
            .await
        {
            Ok(()) => Ok(true),
            Err(Error::Rejected(_)) => {
                self.sensitive = sensitive;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn run(&mut self, mut request: Request) -> Result<Reply> {
        request
            .action
            .validate()
            .map_err(|_| Error::Rejected("Invalid browser arguments."))?;
        if self.sensitive && request.action.read_only() {
            return Err(Error::Rejected(
                "Browser reads are hidden after secure fill.",
            ));
        }
        self.refresh_epoch(request.epoch).await?;
        if let Action::Tabs {
            action, tab, url, ..
        } = &request.action
        {
            return self
                .tab_action(action.clone(), tab.clone(), url.clone())
                .await;
        }
        if let Action::Wait {
            text,
            r#ref,
            timeout_ms,
            ..
        } = &request.action
        {
            return self
                .wait(text.clone(), r#ref.clone(), timeout_ms.unwrap_or(5000))
                .await;
        }
        let page = self.active_page().await.map_err(preflight)?;
        if !page.allowed && !matches!(&request.action, Action::Navigate { .. } | Action::Back) {
            return Err(Error::Rejected(
                "This Chrome page requires Computer pixel tools.",
            ));
        }
        let session = Some(page.session.as_str());
        let mut result = json!({});
        let clear_hover =
            !request.action.read_only() && !matches!(request.action, Action::Hover { .. });
        match request.action {
            Action::Snapshot => return self.snapshot(&page).await,
            Action::Click { r#ref } => {
                self.on_element(&page, &r#ref, "click", Value::Null, false, false)
                    .await?;
            }
            Action::Type {
                r#ref,
                text,
                submit,
            } => {
                self.type_into_ref(&page, &r#ref, text, false, submit)
                    .await?
            }
            Action::Select { r#ref, value } => {
                self.on_element(&page, &r#ref, "select", value.into(), false, false)
                    .await?;
            }
            Action::Hover { r#ref } => self.hover(&page, &r#ref).await?,
            Action::Press { key } => self.press(&page, &key).await?,
            Action::Navigate { url } => {
                self.invalidate();
                let response = self
                    .cdp
                    .call(session, "Page.navigate", json!({"url":url}))
                    .await?;
                if response["errorText"].is_string() {
                    result["message"] = "Chrome could not load the URL.".into();
                }
            }
            Action::Back => {
                let history = self
                    .cdp
                    .call(session, "Page.getNavigationHistory", json!({}))
                    .await?;
                let entry = history["currentIndex"]
                    .as_u64()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| history["entries"].get(index as usize))
                    .ok_or(Error::Rejected("No previous page."))?;
                if entry["url"] != "about:blank"
                    && entry["url"]
                        .as_str()
                        .is_none_or(|url| http_url(url).is_err())
                {
                    return Err(Error::Rejected(
                        "Previous page requires Computer pixel tools.",
                    ));
                }
                self.invalidate();
                self.cdp
                    .call(
                        session,
                        "Page.navigateToHistoryEntry",
                        json!({"entryId":entry["id"]}),
                    )
                    .await?;
            }
            Action::Extract { r#ref } => {
                result = match r#ref {
                    Some(reference) => {
                        self.on_element(&page, &reference, "extract", Value::Null, false, false)
                            .await?
                    }
                    None => self.page_dom(&page, "extract", Value::Null).await?,
                };
            }
            Action::Screenshot { r#ref } => return self.screenshot(&page, r#ref.as_deref()).await,
            Action::Tabs { .. } | Action::Wait { .. } => return Err(Error::Command),
        }
        if clear_hover {
            self.clear_hover().await;
        }
        Ok(reply(result))
    }

    async fn snapshot(&mut self, page: &Page) -> Result<Reply> {
        self.invalidate();
        let tree = self
            .cdp
            .call(
                Some(&page.session),
                "Accessibility.getFullAXTree",
                json!({"depth":50}),
            )
            .await?;
        let state = self.page_dom(page, "verify", Value::Null).await?;
        if state["valid"] != true
            || self.cdp.events(&page.session, |events| events.navigation)? != page.navigation
        {
            return Err(STALE);
        }
        let nodes = tree["nodes"].as_array().ok_or(Error::Command)?;
        let root = nodes.first().ok_or(Error::Command)?;
        let by_id: HashMap<_, _> = nodes
            .iter()
            .filter_map(|node| node["nodeId"].as_str().map(|id| (id, node)))
            .collect();
        let mut elements = HashMap::new();
        let mut lines = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = vec![(root, 0usize)];
        let mut budget = 14_000usize;
        let mut truncated = false;
        let nonce = &Uuid::new_v4().simple().to_string()[..16];
        while let Some((node, level)) = queue.pop() {
            if !visited.insert(node["nodeId"].as_str().ok_or(Error::Command)?) {
                continue;
            }
            let depth = level + usize::from(node["ignored"] != true);
            for id in node["childIds"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .filter_map(Value::as_str)
            {
                if let Some(child) = by_id.get(id) {
                    queue.push((*child, depth));
                } else {
                    truncated = true;
                }
            }
            if node["ignored"] == true {
                continue;
            }
            let role = node["role"]["value"].as_str().unwrap_or_default();
            let name = node["name"]["value"].as_str().unwrap_or_default();
            let actionable = !matches!(
                role,
                "RootWebArea" | "Iframe" | "InlineTextBox" | "StaticText"
            ) && (matches!(
                role,
                "button"
                    | "link"
                    | "textbox"
                    | "searchbox"
                    | "combobox"
                    | "listbox"
                    | "checkbox"
                    | "radio"
                    | "switch"
                    | "tab"
                    | "menuitem"
                    | "menuitemcheckbox"
                    | "menuitemradio"
                    | "slider"
                    | "spinbutton"
                    | "option"
            ) || node["properties"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|p| p["name"] == "focusable" && p["value"]["value"] == true));
            if !actionable
                && name.is_empty()
                && !matches!(
                    role,
                    "RootWebArea"
                        | "main"
                        | "navigation"
                        | "form"
                        | "list"
                        | "listitem"
                        | "table"
                        | "row"
                )
            {
                continue;
            }
            let role = compact(role, 40);
            let name = compact(name, 220);
            let reference = node["backendDOMNodeId"]
                .as_u64()
                .filter(|_| actionable)
                .map(|id| (format!("e{nonce}-{}", elements.len() + 1), id));
            let suffix = reference
                .as_ref()
                .map(|(r, _)| format!(" [ref={r}]"))
                .unwrap_or_default();
            let line = format!("{}{role} {name}{suffix}", "  ".repeat(level.min(10)));
            if line.len() > budget || lines.len() >= 180 {
                truncated = true;
                break;
            }
            budget -= line.len();
            if let Some((reference, id)) = reference {
                elements.insert(reference, id);
            }
            lines.push(line);
        }
        self.snapshot = Some(Snapshot {
            target: page.target.clone(),
            document: page.document.clone(),
            stamp: page.stamp.clone(),
            navigation: page.navigation,
            elements,
        });
        Ok(reply(
            json!({"tab":self.tabs[&page.target].id,"tree":lines.join("\n"),"truncated":truncated}),
        ))
    }

    async fn clear_hover(&mut self) {
        if let Some((session, node)) = self.hovered.take() {
            let _ = self
                .cdp
                .call(
                    Some(&session),
                    "CSS.forcePseudoState",
                    json!({"nodeId":node,"forcedPseudoClasses":[]}),
                )
                .await;
        }
    }

    async fn hover(&mut self, page: &Page, reference: &str) -> Result<()> {
        let backend = self.backend(page, reference)?;
        let session = Some(page.session.as_str());
        for method in ["DOM.enable", "CSS.enable", "DOM.getDocument"] {
            self.cdp
                .call(session, method, json!({}))
                .await
                .map_err(preflight)?;
        }
        let nodes = self
            .cdp
            .call(
                session,
                "DOM.pushNodesByBackendIdsToFrontend",
                json!({"backendNodeIds":[backend]}),
            )
            .await
            .map_err(preflight)?;
        let node = nodes["nodeIds"][0]
            .as_u64()
            .filter(|id| *id != 0)
            .ok_or(STALE)?;
        self.clear_hover().await;
        self.on_element(page, reference, "hover", Value::Null, false, false)
            .await?;
        self.cdp
            .call(
                session,
                "CSS.forcePseudoState",
                json!({"nodeId":node,"forcedPseudoClasses":["hover"]}),
            )
            .await?;
        self.hovered = Some((page.session.clone(), json!(node)));
        Ok(())
    }

    async fn press(&mut self, page: &Page, key: &str) -> Result<()> {
        let (modifiers, key, code, vk) = key_code(key).ok_or(Error::Command)?;
        let text = if modifiers & 7 != 0 {
            ""
        } else if key.len() == 1 {
            &key
        } else if key == "Enter" {
            "\r"
        } else {
            ""
        };
        let mut params = json!({
            "type":if text.is_empty() {"rawKeyDown"} else {"keyDown"}, "key":key, "code":code,
            "windowsVirtualKeyCode":vk,"nativeVirtualKeyCode":vk,"modifiers":modifiers,"text":text,
        });
        self.cdp
            .call(
                Some(&page.session),
                "Input.dispatchKeyEvent",
                params.clone(),
            )
            .await?;
        params["type"] = "keyUp".into();
        params["text"] = "".into();
        self.cdp
            .call(Some(&page.session), "Input.dispatchKeyEvent", params)
            .await?;
        Ok(())
    }

    async fn tab_action(
        &mut self,
        action: TabAction,
        tab: Option<String>,
        url: Option<String>,
    ) -> Result<Reply> {
        let target = tab
            .as_ref()
            .and_then(|id| self.tabs.iter().find(|(_, tab)| &tab.id == id))
            .map(|(target, _)| target.clone());
        let result =
            match action {
                TabAction::List => {
                    let active = self.active_page().await.ok();
                    let mut tabs = self.tabs.iter().map(|(target, tab)| json!({
                    "id":tab.id,"title":compact(&tab.title,160),"url":compact(&tab.url,500),
                    "active":active.as_ref().is_some_and(|p| &p.target == target)
                })).collect::<Vec<_>>();
                    tabs.sort_unstable_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
                    json!({"profile":"Default","tabs":tabs})
                }
                TabAction::Open => {
                    if self.tabs.len() >= MAX_TABS {
                        return Err(Error::Rejected("Too many Chrome tabs. Close a tab first."));
                    }
                    let id = tab_id();
                    let target = self
                        .open(url.as_deref().unwrap_or("about:blank"), &id)
                        .await?;
                    self.activate(&target).await?;
                    json!({"tab":id})
                }
                TabAction::Switch => {
                    self.activate(&target.ok_or(Error::Rejected("Unknown browser tab."))?)
                        .await?;
                    json!({})
                }
                TabAction::Close => {
                    let target = target.ok_or(Error::Rejected("Unknown browser tab."))?;
                    if self.tabs.len() <= 1 {
                        return Err(Error::Rejected("Keep at least one Chrome tab open."));
                    }
                    let result = self
                        .cdp
                        .call(None, "Target.closeTarget", json!({"targetId":target}))
                        .await?;
                    if result["success"] != true {
                        return Err(Error::Command);
                    }
                    if let Some(tab) = self.tabs.remove(&target) {
                        self.cdp.forget(&tab.session);
                    }
                    if self.active.as_ref() == Some(&target) {
                        self.active = None;
                    }
                    self.invalidate();
                    let _ = self.active_page().await;
                    json!({})
                }
            };
        self.save_tabs()?;
        Ok(reply(result))
    }

    async fn wait(
        &mut self,
        text: Option<String>,
        reference: Option<String>,
        ms: u64,
    ) -> Result<Reply> {
        let result = timeout(Duration::from_millis(ms), async {
            loop {
                let page = match self.active_page().await {
                    Ok(page) => page,
                    Err(Error::Rejected(_)) => {
                        self.refresh().await?;
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if !page.allowed {
                    return Err(Error::Rejected(
                        "This Chrome page requires Computer pixel tools.",
                    ));
                }
                let checked = if let Some(text) = &text {
                    self.page_dom(&page, "text", json!(text))
                        .await
                        .map(|value| value["found"] == true)
                } else if let Some(reference) = &reference {
                    self.on_element(&page, reference, "visible", Value::Null, false, false)
                        .await
                        .map(|value| value["found"] == true)
                } else {
                    self.cdp.events(&page.session, |events| events.idle())
                };
                match checked {
                    Ok(true) => return Ok(reply(json!({"matched":true}))),
                    Err(Error::Rejected(_)) if reference.is_none() => {}
                    Ok(false) => {}
                    Err(error) => return Err(error),
                }
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        result.unwrap_or(Err(Error::Rejected("Browser wait timed out.")))
    }

    async fn screenshot(&mut self, page: &Page, reference: Option<&str>) -> Result<Reply> {
        let mut params = json!({"format":"png","captureBeyondViewport":reference.is_some()});
        if let Some(reference) = reference {
            let mut clip = self
                .on_element(page, reference, "clip", Value::Null, false, false)
                .await?;
            clip["scale"] = 1.into();
            params["clip"] = clip;
        }
        let result = self
            .cdp
            .call(
                Some(&page.session),
                "Page.captureScreenshot",
                params.clone(),
            )
            .await?;
        if let Some(reference) = reference {
            let mut current = self
                .on_element(page, reference, "clip", Value::Null, false, false)
                .await?;
            current["scale"] = 1.into();
            if current != params["clip"] {
                return Err(STALE);
            }
        } else {
            self.page_dom(page, "verify", Value::Null).await?;
        }
        let data = result["data"]
            .as_str()
            .filter(|data| data.len() <= 8 * 1024 * 1024)
            .ok_or(Error::Rejected(
                "Browser screenshot exceeds its size limit.",
            ))?;
        Ok(Reply {
            result: json!({"status":"done"}),
            image: Some(data.into()),
        })
    }
}

async fn targets(cdp: &mut Connection) -> Result<Vec<Target>> {
    let targets = cdp
        .call(
            None,
            "Target.getTargets",
            json!({"filter":[{"type":"page"}]}),
        )
        .await?;
    targets["targetInfos"]
        .as_array()
        .ok_or(Error::Command)?
        .iter()
        .map(|v| serde_json::from_value(v.clone()).map_err(|_| Error::Command))
        .collect()
}

fn reply(mut result: Value) -> Reply {
    result["status"] = "done".into();
    Reply {
        result,
        image: None,
    }
}

fn tab_id() -> String {
    format!("t{}", Uuid::new_v4().simple())
}

fn compact(s: &str, limit: usize) -> String {
    s.chars()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn preflight(error: Error) -> Error {
    match error {
        Error::Rejected(_) => error,
        _ => Error::Rejected(
            "Chrome could not identify the page. Refresh the snapshot or use Computer pixels.",
        ),
    }
}
