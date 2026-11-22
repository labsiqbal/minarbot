//! File scope and hashing adapted from colbymchenry/codegraph v1.6.2 (6560052),
//! src/extraction/index.ts, src/file-limits.ts, and codegraph-kernel/src/lib.rs.
//! License: MIT (Copyright (c) 2026 Colby Mchenry).

use crate::{buffers::Tables, generated, langs, stack, store, Error, FileIssue, Result};
use ignore::{
    gitignore::{Gitignore, GitignoreBuilder},
    WalkBuilder,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
};

const MAX_BYTES: u64 = 1024 * 1024;
const DEFAULT_DIRS: &[&str] = &[
    "node_modules/",
    "bower_components/",
    "jspm_packages/",
    "web_modules/",
    ".yarn/",
    ".pnpm-store/",
    ".next/",
    ".nuxt/",
    ".svelte-kit/",
    ".turbo/",
    ".vite/",
    ".parcel-cache/",
    ".angular/",
    ".docusaurus/",
    "storybook-static/",
    ".vinxi/",
    ".nitro/",
    "out-tsc/",
    ".vercel/",
    ".netlify/",
    ".wrangler/",
    "dist/",
    "build/",
    "out/",
    ".output/",
    "coverage/",
    ".nyc_output/",
    "__pycache__/",
    "__pypackages__/",
    ".venv/",
    "venv/",
    ".pixi/",
    ".pdm-build/",
    ".mypy_cache/",
    ".pytest_cache/",
    ".ruff_cache/",
    ".tox/",
    ".nox/",
    ".hypothesis/",
    ".ipynb_checkpoints/",
    ".eggs/",
    "target/",
    ".gradle/",
    "obj/",
    "vendor/",
    ".build/",
    "Pods/",
    "Carthage/",
    "DerivedData/",
    ".swiftpm/",
    ".dart_tool/",
    ".pub-cache/",
    ".cxx/",
    ".externalNativeBuild/",
    "vcpkg_installed/",
    ".bloop/",
    ".metals/",
    "lua_modules/",
    ".luarocks/",
    "__history/",
    "__recovery/",
    ".cache/",
    "*.egg-info/",
    "cmake-build-*/",
    "bazel-*/",
];

pub(crate) struct FilePath {
    pub path: String,
    pub language: &'static str,
}

pub(crate) struct Source {
    pub hash: String,
    pub size: u64,
    pub modified_at: i64,
    pub generated: bool,
    pub graph: Tables,
    pub issue: Option<FileIssue>,
}

pub(crate) enum Outcome {
    Missing,
    Unchanged,
    Changed(Source),
}

struct Defaults {
    patterns: Gitignore,
    project: Gitignore,
}

impl Defaults {
    fn new(root: &Path) -> Result<Self> {
        let mut defaults = GitignoreBuilder::new(root);
        for pattern in DEFAULT_DIRS {
            defaults.add_line(None, pattern)?;
        }
        let mut project = GitignoreBuilder::new(root);
        let gitignore = root.join(".gitignore");
        if gitignore.is_file() {
            if let Some(error) = project.add(gitignore) {
                return Err(error.into());
            }
        }
        Ok(Self {
            patterns: defaults.build()?,
            project: project.build()?,
        })
    }

    fn ignores(&self, path: &Path, is_dir: bool) -> bool {
        if path.components().any(|part| {
            matches!(part, Component::Normal(name) if name == ".git"
                || name == ".codegraph" || name.to_str().is_some_and(|name| name.starts_with(".codegraph-")))
        }) {
            return true;
        }
        self.patterns
            .matched_path_or_any_parents(path, is_dir)
            .is_ignore()
            && !self
                .project
                .matched_path_or_any_parents(path, is_dir)
                .is_whitelist()
    }
}

pub(crate) fn discover(root: &Path) -> Result<Vec<FilePath>> {
    let mut paths = BTreeSet::new();
    let defaults = Defaults::new(root)?;
    let in_worktree = match git(root, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(output) => output.status.success() && output.stdout.starts_with(b"true"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(source) => return Err(source_error(root, source)),
    };
    // Repositories nested in an ignored parent path still need a local walk.
    let ignored_root = in_worktree
        && git(root, &["check-ignore", "--quiet", "."]).is_ok_and(|output| output.status.success());
    if in_worktree && !ignored_root {
        git_paths(root, Path::new(""), &defaults, &mut paths)?;
    } else {
        let filter_root = root.to_owned();
        let mut walk = WalkBuilder::new(root);
        walk.hidden(false)
            .require_git(false)
            .parents(false)
            .ignore(false)
            .filter_entry(move |entry| {
                let Ok(relative) = entry.path().strip_prefix(&filter_root) else {
                    return false;
                };
                relative.as_os_str().is_empty()
                    || !defaults.ignores(
                        relative,
                        entry.file_type().is_some_and(|kind| kind.is_dir()),
                    )
            });
        for entry in walk.build() {
            let entry = entry?;
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                if let Ok(relative) = entry.path().strip_prefix(root) {
                    paths.insert(relative.to_owned());
                }
            }
        }
    }
    Ok(paths
        .into_iter()
        .filter_map(|path| {
            Some(FilePath {
                language: langs::detect(&path)?,
                path: path
                    .components()
                    .map(|part| part.as_os_str().to_str())
                    .collect::<Option<Vec<_>>>()?
                    .join("/"),
            })
        })
        .collect())
}

fn git(root: &Path, args: &[&str]) -> io::Result<Output> {
    Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
}

fn git_paths(
    root: &Path,
    prefix: &Path,
    defaults: &Defaults,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let directory = root.join(prefix);
    let cached = git(
        &directory,
        &["ls-files", "-z", "--stage", "--recurse-submodules"],
    )
    .map_err(|source| source_error(&directory, source))?;
    let cached = if cached.status.success() {
        cached
    } else {
        git(&directory, &["ls-files", "-z", "--stage"])
            .map_err(|source| source_error(&directory, source))?
    };
    let untracked = git(
        &directory,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )
    .map_err(|source| source_error(&directory, source))?;
    if let Some(failed) = [&cached, &untracked]
        .into_iter()
        .find(|output| !output.status.success())
    {
        return Err(source_error(
            &directory,
            io::Error::other(format!(
                "git ls-files: {}",
                String::from_utf8_lossy(&failed.stderr).trim()
            )),
        ));
    }

    let mut embedded = BTreeSet::new();
    let tracked = nul_paths(&cached).filter_map(|record| {
        let (fields, name) = record.split_once('\t')?;
        if fields.starts_with("160000 ") {
            Some((name, true))
        } else if fields.starts_with("100") {
            Some((name, false))
        } else {
            None
        }
    });
    let untracked = nul_paths(&untracked).map(|name| (name, name.ends_with('/')));
    for (name, is_repo) in tracked.chain(untracked) {
        let path = Path::new(name);
        if name.is_empty()
            || !path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            continue;
        }
        let full = prefix.join(path);
        if is_repo {
            let child = root.join(&full);
            // A real embedded repository gets its own Git scope. Linked
            // worktrees are duplicate views and have a .git file.
            if !defaults.ignores(&full, true)
                && child
                    .join(".git")
                    .symlink_metadata()
                    .is_ok_and(|meta| meta.is_dir())
                && child.canonicalize().is_ok_and(|resolved| resolved == child)
            {
                embedded.insert(full);
            }
        } else if langs::detect(path).is_some() && !defaults.ignores(&full, false) {
            paths.insert(full);
        }
    }
    for child in embedded {
        git_paths(root, &child, defaults, paths)?;
    }
    Ok(())
}

fn nul_paths(output: &Output) -> impl Iterator<Item = &str> {
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok())
}

fn source_error(path: &Path, source: io::Error) -> Error {
    Error::Source {
        path: path.to_owned(),
        source,
    }
}

pub(crate) fn extract(
    root: &Path,
    file: &FilePath,
    previous: Option<&(String, String)>,
) -> Result<Outcome> {
    let path = root.join(&file.path);
    let read = read_source(&path).map_err(|source| source_error(&path, source))?;
    let Some((size, modified_at, bytes)) = read else {
        return Ok(Outcome::Missing);
    };
    let text = String::from_utf8_lossy(&bytes);
    let oversized = size > MAX_BYTES;
    let hash = if oversized {
        format!("{:x}", Sha256::digest(format!("codegraph:oversize:{size}")))
    } else {
        format!("{:x}", Sha256::digest(text.as_bytes()))
    };
    if previous.is_some_and(|(before, language)| before == &hash && language == file.language) {
        return Ok(Outcome::Unchanged);
    }

    let (graph, issue) = if oversized {
        (
            Tables::default(),
            Some((
                "size_exceeded",
                format!("File exceeds max size ({size} > {MAX_BYTES})"),
            )),
        )
    } else {
        let result = stack::run_guarded(|| match file.language {
            "go" => crate::go::extract(&file.path, &text),
            "rust" => crate::rustlang::extract(&file.path, &text),
            "python" => crate::python::extract(&file.path, &text),
            language => crate::tsjs::extract(&file.path, &text, language),
        });
        match result {
            Ok(graph) => (graph, None),
            Err(error) => (Tables::default(), Some(("parse_error", error))),
        }
    };
    Ok(Outcome::Changed(Source {
        hash,
        size,
        modified_at,
        graph,
        generated: generated::detect(&file.path, &text),
        issue: issue.map(|(code, message)| FileIssue {
            path: file.path.clone(),
            code,
            message,
        }),
    }))
}

pub(crate) fn read_text(path: &Path) -> Option<String> {
    let (size, _, bytes) = read_source(path).ok()??;
    (size <= MAX_BYTES).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn read_source(path: &Path) -> io::Result<Option<(u64, i64, Vec<u8>)>> {
    let result = (|| -> io::Result<_> {
        // Neither Git symlinks nor a file redirected outside the project are sources.
        if path.canonicalize()? != path || !path.symlink_metadata()?.is_file() {
            return Ok(None);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        let stat = file.metadata()?;
        if !stat.is_file() {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        let size = if stat.len() > MAX_BYTES {
            stat.len()
        } else {
            (&file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
            let size = (bytes.len() as u64).max(file.metadata()?.len());
            if size > MAX_BYTES {
                bytes.clear();
                size
            } else {
                bytes.len() as u64
            }
        };
        Ok(Some((
            size,
            store::timestamp(file.metadata()?.modified()?),
            bytes,
        )))
    })();
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        other => other,
    }
}
