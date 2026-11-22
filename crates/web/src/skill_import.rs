use crate::{connection_ui::*, skill_files};
use base64::{engine::general_purpose::STANDARD, Engine};
use leptos::{html, prelude::*, task::spawn_local};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, File, FileList, HtmlAnchorElement, HtmlInputElement, Url};

const MIB: usize = 1024 * 1024;
const BINARY: &str = "minarbot-binary-v1";
const ZIP_ERROR: &str =
    "ZIP is invalid or uses an unsupported format. Use a standard ZIP with UTF-8 SKILL.md.";

fn from_bytes(path: &str, bytes: &[u8], binary: bool) -> Value {
    if let Ok(text) = std::str::from_utf8(bytes) {
        if !binary && !text.bytes().any(|b| matches!(b, 0..=8 | 11 | 12 | 14..=31)) {
            return json!({ "path": path, "content": text });
        }
    }
    json!({ "path": path, "content": STANDARD.encode(bytes), "encoding": "base64" })
}

fn read_zip(bytes: &[u8]) -> Result<Value, String> {
    use zip::{CompressionMethod, ZipArchive};
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|_| ZIP_ERROR)?;
    if !(1..=64).contains(&archive.len()) || archive.offset() != 0 {
        return Err(ZIP_ERROR.into());
    }
    let mut rows = Vec::new();
    let mut paths = BTreeSet::new();
    let mut total = 0;
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(|_| ZIP_ERROR)?;
        let path = std::str::from_utf8(file.name_raw()).map_err(|_| ZIP_ERROR)?;
        let directory = file.is_dir();
        let normalized = path.strip_suffix('/').unwrap_or(path).to_lowercase();
        if normalized
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
            || path.encode_utf16().count() > 305
            || path
                .chars()
                .any(|c| c == ':' || c == '\\' || c <= '\x1f' || c == '\x7f')
            || !paths.insert(normalized)
            || !matches!(
                file.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
            || file.unix_mode().is_some_and(|mode| {
                let kind = mode & 0o170000;
                kind != 0 && kind != if directory { 0o040000 } else { 0o100000 }
            })
        {
            return Err(ZIP_ERROR.into());
        }
        let size = usize::try_from(file.size()).map_err(|_| ZIP_ERROR)?;
        if size > 2 * MIB || total + size > 2 * MIB + 48000 || directory && size != 0 {
            return Err(ZIP_ERROR.into());
        }
        total += size;
        let path = path.to_owned();
        let binary = file.comment() == BINARY;
        let mut data = Vec::with_capacity(size);
        file.take(size as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| ZIP_ERROR)?;
        if data.len() != size {
            return Err(ZIP_ERROR.into());
        }
        if !directory {
            rows.push(from_bytes(&path, &data, binary));
        }
    }
    let files = rows
        .iter()
        .filter_map(|file| file["path"].as_str())
        .map(str::to_lowercase)
        .collect::<BTreeSet<_>>();
    if paths.iter().any(|path| {
        path.match_indices('/')
            .any(|(n, _)| files.contains(&path[..n]))
    }) {
        return Err(ZIP_ERROR.into());
    }
    let mut root = None;
    if !rows.iter().any(|row| row["path"] == "SKILL.md") {
        let folder = rows
            .first()
            .and_then(|row| row["path"].as_str())
            .and_then(|path| path.split_once('/'))
            .map(|(folder, _)| folder.to_owned())
            .ok_or(ZIP_ERROR)?;
        let prefix = format!("{folder}/");
        for row in &mut rows {
            let path = row["path"]
                .as_str()
                .and_then(|path| path.strip_prefix(&prefix))
                .ok_or(ZIP_ERROR)?;
            row["path"] = json!(path);
        }
        root = Some(folder);
    }
    let parsed = skill_files::parse_files(&json!(rows))?;
    if root.is_some_and(|root| parsed["name"] != root) {
        return Err("ZIP folder name must match the name in SKILL.md.".into());
    }
    Ok(parsed)
}

fn write_zip(name: &str, files: &Value) -> Result<Vec<u8>, String> {
    // ponytail: stored entries retain binary comments; ZIP64 and spanning are outside the skill limits.
    let mut output = Vec::new();
    let mut central = Vec::new();
    let files = files.as_array().ok_or(ZIP_ERROR)?;
    for file in files {
        let path = format!("{name}/{}", file["path"].as_str().ok_or(ZIP_ERROR)?);
        let content = file["content"].as_str().ok_or(ZIP_ERROR)?;
        let binary = file["encoding"] == "base64";
        let bytes = if binary {
            STANDARD.decode(content).map_err(|_| ZIP_ERROR)?
        } else {
            content.as_bytes().to_vec()
        };
        let comment = if binary { BINARY } else { "" };
        let offset = output.len() as u32;
        let size = bytes.len() as u32;
        let crc = crc32fast::hash(&bytes);
        output.extend_from_slice(b"PK\x03\x04");
        output.extend_from_slice(&[20, 0, 0, 8, 0, 0, 0, 0, 33, 0]);
        for value in [crc, size, size] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        output.extend_from_slice(&(path.len() as u16).to_le_bytes());
        output.extend_from_slice(&0_u16.to_le_bytes());
        output.extend_from_slice(path.as_bytes());
        output.extend_from_slice(&bytes);

        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&[20, 0, 20, 0, 0, 8, 0, 0, 0, 0, 33, 0]);
        for value in [crc, size, size] {
            central.extend_from_slice(&value.to_le_bytes());
        }
        for value in [path.len() as u16, 0, comment.len() as u16, 0, 0] {
            central.extend_from_slice(&value.to_le_bytes());
        }
        central.extend_from_slice(&0_u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(path.as_bytes());
        central.extend_from_slice(comment.as_bytes());
    }
    let central_start = output.len() as u32;
    output.extend_from_slice(&central);
    output.extend_from_slice(b"PK\x05\x06");
    for value in [0, 0, files.len() as u16, files.len() as u16] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output.extend_from_slice(&(central.len() as u32).to_le_bytes());
    output.extend_from_slice(&central_start.to_le_bytes());
    output.extend_from_slice(&0_u16.to_le_bytes());
    Ok(output)
}

pub fn download(files: &Value, zip: bool) -> Result<(), String> {
    let parsed = skill_files::parse_files(files)?;
    let name = parsed["name"].as_str().ok_or(ZIP_ERROR)?;
    let (name, bytes, mime) = if zip {
        (
            format!("{name}.zip"),
            write_zip(name, files)?,
            "application/zip",
        )
    } else {
        (
            "minarbot-skill-folder.json".into(),
            json!({ "format": "minarbot-skill-folder", "version": 1, "files": files })
                .to_string()
                .into_bytes(),
            "application/json",
        )
    };
    let save = || -> Result<(), JsValue> {
        let data = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes.as_slice()));
        let options = BlobPropertyBag::new();
        options.set_type(mime);
        let blob = Blob::new_with_u8_array_sequence_and_options(&data, &options)?;
        let url = Url::create_object_url_with_blob(&blob)?;
        let anchor = document()
            .create_element("a")?
            .dyn_into::<HtmlAnchorElement>()?;
        anchor.set_href(&url);
        anchor.set_download(&name);
        anchor.click();
        set_timeout(
            move || {
                let _ = Url::revoke_object_url(&url);
            },
            std::time::Duration::from_secs(1),
        );
        Ok(())
    };
    save().map_err(|_| "Skill folder could not be downloaded.".into())
}

async fn bytes(file: &File) -> Result<Vec<u8>, &'static str> {
    let data = JsFuture::from(file.array_buffer())
        .await
        .map_err(|_| "Files could not be read.")?;
    Ok(js_sys::Uint8Array::new(&data).to_vec())
}

fn relative(file: &File) -> Option<String> {
    js_sys::Reflect::get(file, &JsValue::from_str("webkitRelativePath"))
        .ok()?
        .as_string()
        .filter(|path| !path.is_empty())
}

async fn import(files: Vec<File>) -> Result<Value, String> {
    let first = files.first().ok_or("No files selected.")?;
    let folder = relative(first);
    let zipped =
        files.len() == 1 && folder.is_none() && first.name().to_lowercase().ends_with(".zip");
    if files.len() > 32
        || files.iter().map(|file| file.size()).sum::<f64>()
            > (if zipped { 3 * MIB } else { 4 * MIB }) as f64
    {
        return Err("Choose up to 32 files totaling 4 MiB, or one ZIP up to 3 MiB.".into());
    }
    if zipped {
        return read_zip(&bytes(first).await?);
    }
    if files.len() == 1 && folder.is_none() && first.name().ends_with(".json") {
        let data: Value = serde_json::from_slice(&bytes(first).await?)
            .map_err(|_| "File is not a Minarbot version 1 skill folder export.")?;
        if data["format"] != "minarbot-skill-folder"
            || data["version"] != 1
            || !data.as_object().is_some_and(|data| {
                data.keys()
                    .all(|key| matches!(key.as_str(), "format" | "version" | "files"))
            })
        {
            return Err("File is not a Minarbot version 1 skill folder export.".into());
        }
        return skill_files::parse_files(&data["files"]);
    }
    let mut rows = Vec::new();
    let root = folder
        .as_deref()
        .and_then(|path| path.split_once('/'))
        .map(|(root, _)| root);
    for file in files {
        let path = relative(&file)
            .and_then(|path| path.split_once('/').map(|(_, path)| path.to_owned()))
            .unwrap_or_else(|| file.name());
        rows.push(from_bytes(&path, &bytes(&file).await?, false));
    }
    let parsed = skill_files::parse_files(&json!(rows))?;
    if root.is_some_and(|root| parsed["name"] != root) {
        return Err("Folder name must match the name in SKILL.md.".into());
    }
    Ok(parsed)
}

#[component]
pub fn Importer(
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    result: Callback<Value>,
    #[prop(optional, into)] disabled: Signal<bool>,
    #[prop(optional)] compact: bool,
) -> impl IntoView {
    let folder = NodeRef::<html::Input>::new();
    let file = NodeRef::<html::Input>::new();
    folder.on_load(|element| {
        let _ = element.set_attribute("webkitdirectory", "");
    });
    let read = move |event: web_sys::Event| {
        if busy.get_untracked() || disabled.get_untracked() {
            return;
        }
        let Some(input) = event
            .target()
            .and_then(|node| node.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        let list: FileList = match input.files() {
            Some(list) => list,
            None => return,
        };
        let files = (0..list.length())
            .filter_map(|index| list.item(index))
            .collect::<Vec<_>>();
        input.set_value("");
        if files.is_empty() {
            return;
        }
        error.set(String::new());
        busy.set(true);
        spawn_local(async move {
            let value = import(files).await;
            if busy.try_set(false).is_some() {
                return;
            }
            match value {
                Ok(parsed) => result.run(parsed),
                Err(message) => error.set(message),
            }
        });
    };
    let pick_folder = UiAction::new(move |()| {
        if let Some(input) = folder.get_untracked() {
            input.click();
        }
    });
    let pick_file = UiAction::new(move |()| {
        if let Some(input) = file.get_untracked() {
            input.click();
        }
    });
    view! {
        <input type="file" multiple hidden node_ref=folder on:change=read />
        <input type="file" accept=".md,.json,.zip" hidden node_ref=file on:change=read />
        {if compact {
            view! {
                <Button label="Import folder" size="sm" variant="ghost" disabled=disabled busy=busy click=pick_folder />
                <Button label="Import file" size="sm" variant="ghost" disabled=disabled busy=busy click=pick_file />
            }.into_any()
        } else {
            view! { <Menu label="Import" text=true disabled=move || disabled.get() || busy.get()
                items=vec![MenuItem::new("Folder", pick_folder), MenuItem::new("File", pick_file)] /> }.into_any()
        }}
    }
}
