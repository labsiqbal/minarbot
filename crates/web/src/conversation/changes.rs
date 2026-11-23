use super::*;

#[derive(Clone, PartialEq, Deserialize)]
pub(super) struct Status {
    #[serde(rename = "isRepo")]
    pub is_repo: bool,
    branch: Option<String>,
    #[serde(rename = "changedFiles", default)]
    pub changed_files: Vec<Changed>,
}
#[derive(Clone, PartialEq, Deserialize)]
pub(super) struct Changed {
    code: String,
    path: String,
}
#[derive(Clone, PartialEq, Deserialize)]
struct Diff {
    text: String,
    truncated: bool,
    #[serde(rename = "binaryFiles", default)]
    binary_files: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Hunk,
    Add,
    Remove,
    Context,
}

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    kind: Kind,
    old: Option<u64>,
    new: Option<u64>,
}

impl Line<'_> {
    fn class(self) -> &'static str {
        match self.kind {
            Kind::Hunk => "is-hunk",
            Kind::Add => "is-add",
            Kind::Remove => "is-remove",
            Kind::Context => "is-context",
        }
    }
}

fn hunk(text: &str) -> Option<(u64, u64)> {
    let (old, text) = text.strip_prefix("@@ -")?.split_once(" +")?;
    let (new, _) = text.split_once(" @@")?;
    let number = |value: &str| value.split(',').next()?.parse().ok();
    Some((number(old)?, number(new)?))
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let mut numbers = None;
    text.lines()
        .filter_map(|text| {
            if text.starts_with("diff --git ") {
                numbers = None;
            }
            if let Some(hunk) = hunk(text) {
                numbers = Some(hunk);
                return Some(Line {
                    text,
                    kind: Kind::Hunk,
                    old: None,
                    new: None,
                });
            }
            let (old, new) = numbers.as_mut()?;
            let (kind, o, n) = match text.as_bytes().first()? {
                b'-' => {
                    let n = *old;
                    *old = old.saturating_add(1);
                    (Kind::Remove, Some(n), None)
                }
                b'+' => {
                    let n = *new;
                    *new = new.saturating_add(1);
                    (Kind::Add, None, Some(n))
                }
                b' ' => {
                    let (o, n) = (*old, *new);
                    *old = old.saturating_add(1);
                    *new = new.saturating_add(1);
                    (Kind::Context, Some(o), Some(n))
                }
                _ => return None,
            };
            Some(Line {
                text,
                kind,
                old: o,
                new: n,
            })
        })
        .collect()
}

fn split_cell(line: Option<Line<'_>>, old: bool) -> impl IntoView {
    let number = line
        .and_then(|line| if old { line.old } else { line.new })
        .map(|number| number.to_string());
    let text = line.map_or(" ", |line| line.text).to_owned();
    view! { <div class=format!("right-panel__split-cell {}", line.map_or("is-empty", Line::class))>
    <span class="git-diff__number">{number}</span><code>{text}</code></div> }
}

fn split(lines: &[Line<'_>]) -> impl IntoView {
    let mut remaining = lines;
    let mut rows = Vec::new();
    while let Some((line, tail)) = remaining.split_first() {
        remaining = tail;
        match line.kind {
            Kind::Hunk => rows.push(view! { <div class="right-panel__split-heading">{line.text.to_owned()}</div> }.into_any()),
            Kind::Context => rows.push(view! { <div class="right-panel__split-row">{split_cell(Some(*line), true)}{split_cell(Some(*line), false)}</div> }.into_any()),
            Kind::Add | Kind::Remove => {
                let change = remaining.iter().take_while(|line| matches!(line.kind, Kind::Add | Kind::Remove)).count();
                let changes = std::iter::once(line).chain(&remaining[..change]);
                let removed = changes.clone().filter(|line| line.kind == Kind::Remove).copied().collect::<Vec<_>>();
                let added = changes.filter(|line| line.kind == Kind::Add).copied().collect::<Vec<_>>();
                for index in 0..removed.len().max(added.len()) {
                    rows.push(view! { <div class="right-panel__split-row">{split_cell(removed.get(index).copied(), true)}{split_cell(added.get(index).copied(), false)}</div> }.into_any());
                }
                remaining = &remaining[change..];
            }
        }
    }
    view! { <div class="right-panel__split" role="region" aria-label="Split diff" tabindex="0"><div>{rows}</div></div> }
}

fn unified(lines: &[Line<'_>]) -> impl IntoView {
    view! { <div class="git-diff" role="region" aria-label="Unified diff" tabindex="0"><div class="git-diff__lines">
        {lines.iter().map(|line| view! { <div class=format!("git-diff__line {}", line.class())>
            <span class="git-diff__number" aria-label=line.old.map(|n| format!("Old line {n}"))>{line.old.map(|n| n.to_string())}</span>
            <span class="git-diff__number" aria-label=line.new.map(|n| format!("New line {n}"))>{line.new.map(|n| n.to_string())}</span>
            <code>{line.text.to_owned()}</code>
        </div> }).collect_view()}
    </div></div> }
}

pub(super) fn panel(chat: Chat, status: RwSignal<Option<Result<Status, String>>>) -> impl IntoView {
    let selected = RwSignal::new(String::new());
    let diff = RwSignal::new(None::<Result<Diff, String>>);
    let refresh = RwSignal::new(0_u32);
    let wrap = RwSignal::new(true);
    let layout = RwSignal::new(String::from("unified"));
    let files = Memo::new(move |_| {
        status.with(|status| {
            status
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .map(|value| value.changed_files.clone())
                .unwrap_or_default()
        })
    });
    let file = Memo::new(move |_| {
        files.with(|files| {
            files
                .iter()
                .find(|file| file.path == selected.get())
                .or_else(|| files.first())
                .map(|file| file.path.clone())
        })
    });
    files::watch(
        move || {
            file.get().map_or_else(String::new, |file| {
                files::url(
                    &format!("{}/panel/git/diff", chat.base()),
                    &[("path", &file)],
                )
            })
        },
        refresh,
        diff,
    );
    let icons = use_context::<Icons>().unwrap_or_default();
    view! {
        <div class="right-panel__back">{files::back(chat, "Back to files")}<span>"Source control"</span></div>
        <div class="right-panel__body k-stack" class:is-wrapped=wrap>
            <section class="k-section"><div class="k-section__head">
                <h2 class="k-section__title"><span class="right-panel__branch"><span class="appearance-icon" inner_html=icons.branch></span>
                    {move || status.with(|status| status.as_ref().and_then(|value| value.as_ref().ok()).and_then(|value| value.branch.clone()).unwrap_or_else(|| "Detached HEAD".into()))}
                </span></h2>
                <div class="k-section__actions"><span class="k-badge is-neutral">{move || format!("{} changed", files.with(Vec::len))}</span></div>
            </div></section>
            {move || files.with(Vec::is_empty).then(|| files::empty("Working tree clean"))}
            <div class="right-panel__changed-files" aria-label="Changed files" hidden=move || files.with(Vec::is_empty)>
                <div class="k-card">
                    <For each=move || files.get() key=|item| (item.path.clone(), item.code.clone()) children=move |item| {
                        let path = StoredValue::new(item.path);
                        let code = item.code.trim();
                        let letter = if code == "??" { "U".into() } else { code.chars().take(1).collect::<String>() };
                        let label = match code.chars().next() {
                            Some('?') => "Untracked", Some('M') => "Modified", Some('A') => "Added", Some('D') => "Deleted",
                            Some('R') => "Renamed", Some('C') => "Copied", Some('U') => "Conflict", _ => code,
                        }.to_owned();
                        view! { <div class="k-row is-clickable"><button type="button" class="k-row__main" on:click=move |_| {
                            if file.get_untracked().as_deref() != Some(&path.get_value()) { diff.set(None); selected.set(path.get_value()); }
                        }><span class="k-row__leading"><span class="source-control__status" title=label>{letter}</span></span><span class="k-row__text">
                            <span class="k-row__title"><span title=path.get_value() aria-current=move || (file.get().as_deref() == Some(&path.get_value())).then_some("true")>{path.get_value()}</span></span>
                        </span></button></div> }
                    } />
                </div>
            </div>
            <div class="right-panel__preview-head" hidden=move || file.with(Option::is_none)>
                <span class="right-panel__path" title=move || file.get()>{file}</span>
                <div class="right-panel__diff-counts" aria-label="Line changes">
                    {move || {
                        let (added, removed) = diff.with(|value| value.as_ref().and_then(|value| value.as_ref().ok()).map_or((0, 0), |diff| {
                            lines(&diff.text).into_iter().fold((0, 0), |(add, remove), line| (add + usize::from(line.kind == Kind::Add), remove + usize::from(line.kind == Kind::Remove)))
                        }));
                        view! { <span>{format!("+{added}")}</span><span>{format!("-{removed}")}</span> }
                    }}
                </div>
            </div>
            <div class="right-panel__diff-toolbar" hidden=move || file.with(Option::is_none)>
                <span class="conversation-select">
                    <select class="k-input k-select k-select--sm" aria-label="Diff layout" prop:value=layout on:change=move |event| layout.set(event_target_value(&event))>
                        <option value="unified">"Unified"</option><option value="split">"Split"</option>
                    </select>
                    <span class="k-select__chev" aria-hidden="true" inner_html=icons.chevron></span>
                </span>
                <button type="button" class="k-icon-btn" aria-label="Wrap lines" title="Wrap lines" aria-pressed=move || wrap.get().to_string() on:click=move |_| wrap.update(|value| *value = !*value)>
                    <span class="appearance-icon" inner_html=icons.wrap></span></button>
            </div>
            {move || if file.with(Option::is_none) { ().into_any() } else {
                match diff.get() {
                    None => files::loading("Loading diff...").into_any(),
                    Some(Err(error)) => files::retry(error, refresh).into_any(),
                    Some(Ok(diff)) => view! {
                        {diff.truncated.then(|| view! { <div class="k-alert is-warn" role="alert"><div class="k-alert__body">"Partial diff. Preview is limited to 256 KiB."</div></div> })}
                        {if !diff.binary_files.is_empty() {
                            view! { <div class="k-empty"><div class="k-empty__title">"Binary file"</div><div class="k-empty__text">"Only text diffs can be displayed."</div></div> }.into_any()
                        } else if diff.text.is_empty() {
                            view! { <div class="k-empty"><div class="k-empty__title">"No changes"</div><div class="k-empty__text">"File matches the current commit."</div></div> }.into_any()
                        } else if layout.get() == "split" { split(&lines(&diff.text)).into_any() }
                        else { unified(&lines(&diff.text)).into_any() }}
                    }.into_any(),
                }
            }}
        </div>
    }
}
