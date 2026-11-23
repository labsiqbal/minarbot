use crate::connection_ui::*;
use leptos::prelude::*;
use serde::Deserialize;

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Status {
    is_repo: bool,
    branch: Option<String>,
    #[serde(default)]
    dirty_count: usize,
    #[serde(default)]
    changed_files: Vec<File>,
    #[serde(default)]
    local_branches: Vec<String>,
    #[serde(default)]
    worktrees: Vec<serde_json::Value>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct File {
    path: String,
    code: String,
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Diff {
    text: String,
    truncated: bool,
    binary_files: Vec<String>,
}

pub fn panel(project: StoredValue<String>, folder: String) -> impl IntoView {
    let base = StoredValue::new(format!("/api/projects/{}/git", project.get_value()));
    let folder = StoredValue::new(encode(&folder));
    let status =
        query::<Status>(move || format!("{}?folder={}", base.get_value(), folder.get_value()));
    let file = RwSignal::new(None::<String>);
    let refresh = UiAction::new(move |()| status.reload.run(()));
    status.poll(5000, || true);
    let show_diff = move || {
        file.get().map(|path| {
        let encoded = encode(&path);
        let diff = query::<Diff>(move || format!("{}/diff?path={encoded}&folder={}", base.get_value(), folder.get_value()));
        view! {
            <Modal title="File diff".into() description=path size="lg" close=UiAction::new(move |()| file.set(None))>
                <ErrorNotice error=diff.error.into() retry=diff.reload label="Retry diff" />
                {move || match diff.data.get() {
                    Some(diff) => view! {
                        {diff.truncated.then(|| view! { <Alert tone="warn">"Partial diff preview. Output is limited to 256 KiB."</Alert> })}
                        {if !diff.binary_files.is_empty() {
                            view! { <Empty title="Binary file">"Only text diffs can be displayed."</Empty> }.into_any()
                        } else if diff.text.is_empty() {
                            view! { <Empty title="No changes">"This file matches the current commit."</Empty> }.into_any()
                        } else { unified_diff(&diff.text).into_any() }}
                    }.into_any(),
                    None if diff.error.with(String::is_empty) => view! { <p role="status">"Loading diff..."</p> }.into_any(),
                    None => ().into_any(),
                }}
            </Modal>
        }
    })
    };
    view! {
        <div class="bs-root source-control">
            {move || {
                let data = status.data.get();
                if !data.as_ref().is_some_and(|row| row.is_repo) && status.error.with(String::is_empty) {
                    return view! { <Section title="Source control"><Empty title=if data.is_some() {
                        "No Git repository in this folder"
                    } else { "Loading source control..." } /></Section> }.into_any();
                }
                view! { <Section title="Source control" actions=Box::new(move || view! {
                    <button class="k-icon-btn" type="button" aria-label="Refresh source control" title="Refresh source control"
                        disabled=status.busy on:click=move |_| refresh.run(())><Icon name="refresh" /></button>
                    }.into_any())>
                    <Show when=move || !status.error.with(String::is_empty)><Alert>{status.error}</Alert></Show>
                    {data.filter(|row| row.is_repo).map(|row| {
                        let branches = row.local_branches.len();
                        let worktrees = row.worktrees.len();
                        view! {
                            <div class="k-card">
                                <Row title=row.branch.unwrap_or_else(|| "Detached HEAD".into()) icon="git-branch"
                                    trailing=Box::new(move || view! { <Badge>{
                                        if row.dirty_count == 0 { "Clean".into() } else { format!("{} changed", row.dirty_count) }
                                    }</Badge> }.into_any()) />
                                <div class="source-control__files" role="group" aria-label="Changed files">
                                    {if row.changed_files.is_empty() {
                                        view! { <Empty title="Working tree clean" /> }.into_any()
                                    } else {
                                        row.changed_files.into_iter().map(|entry| {
                                            let path = entry.path;
                                            let label = if entry.code == "??" { "Untracked".into() } else { format!("Git status {}", entry.code) };
                                            let choose = path.clone();
                                            let title = path.clone();
                                            view! { <div class="k-row is-clickable">
                                                <button class="k-row__main" type="button" on:click=move |_| file.set(Some(choose.clone()))>
                                                    <span class="k-row__leading"><span class="source-control__status" role="img" aria-label=label>{entry.code}</span></span>
                                                    <span class="k-row__text"><span class="k-row__title" title=title>{path}</span></span>
                                                    <span class="k-row__chev"><Icon name="chevron-right" /></span>
                                                </button>
                                            </div> }
                                        }).collect_view().into_any()
                                    }}
                                </div>
                            </div>
                            <p class="source-control__hint">{format!("Local branches: {branches} · Worktrees: {worktrees}")}</p>
                        }
                    })}
                </Section> }.into_any()
            }}
            {show_diff}
        </div>
    }
}

fn unified_diff(text: &str) -> impl IntoView {
    let mut numbers = None;
    let lines = text.strip_suffix('\n').unwrap_or(text).split('\n').map(|line| {
        if line.starts_with("diff --git ") {
            numbers = None;
        }
        let hunk = line.strip_prefix("@@ -").and_then(|line| line.split_once(" +")).and_then(|(old, next)| {
            let old = old.split([',', ' ']).next()?.parse::<u64>().ok()?;
            let next = next.split([',', ' ']).next()?.parse::<u64>().ok()?;
            Some((old, next))
        });
        let (kind, old, next) = if let Some(hunk) = hunk {
            numbers = Some(hunk);
            ("hunk", None, None)
        } else if let Some((old, next)) = numbers.as_mut() {
            match line.as_bytes().first() {
                Some(b'+') => {
                    let number = *next;
                    *next = next.saturating_add(1);
                    ("add", None, Some(number))
                }
                Some(b'-') => {
                    let number = *old;
                    *old = old.saturating_add(1);
                    ("remove", Some(number), None)
                }
                Some(b' ') => {
                    let pair = (*old, *next);
                    *old = old.saturating_add(1);
                    *next = next.saturating_add(1);
                    ("context", Some(pair.0), Some(pair.1))
                }
                _ => ("meta", None, None),
            }
        } else { ("meta", None, None) };
        view! {
            <div class=format!("git-diff__line is-{kind}")>
                <span class="git-diff__number" aria-label=old.map(|line| format!("Old line {line}"))>{old}</span>
                <span class="git-diff__number" aria-label=next.map(|line| format!("New line {line}"))>{next}</span>
                <code>{if line.is_empty() { " ".into() } else { line.to_owned() }}</code>
            </div>
        }
    }).collect_view();
    view! { <div class="git-diff" role="region" aria-label="Unified diff" tabindex="0"><div class="git-diff__lines">{lines}</div></div> }
}
