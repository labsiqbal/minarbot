use super::*;
use crate::{db, harness::process::ProbeHome};
use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::{
    fs,
    sync::{Arc, Mutex},
};

const AGENT: &str = "00112233-4455-4677-8899-aabbccddeeff";
const SOURCE_MARKER: &str = "synthetic proprietary implementation";
const SECRET: &str = "ctx7sk-fixture-secret-1234";

#[test]
fn manifests_resolve_releases_and_skip_nonregistry_sources() -> db::Result<()> {
    let home = ProbeHome::new()?;
    let cases = [
        ("package.json", r#"{"name":"workspace-secret","dependencies":{"react":"^19.1.0","alias":"npm:lodash@^4.17.0"},"devDependencies":{"pin":"2.0.0","local":"file:../private"}}"#),
        ("package-lock.json", r#"{"packages":{"":{"name":"workspace-secret"},"node_modules/react":{"version":"19.1.2"},"node_modules/alias":{"name":"lodash","version":"4.17.21"},"node_modules/private":{"link":true,"version":"1.0.0"}}}"#),
        ("frontend/pnpm-lock.yaml", "packages:\n  '@scope/example@2.1.0': {}\n"),
        ("second/yarn.lock", "\"better@^2.0.0\":\n  version \"2.3.1\"\n"),
        ("third/bun.lock", "{\n  \"packages\": {\n    \"minar-fixture\": [\"minar-fixture@7.5.1\", \"\", {}],\n  },\n}\n"),
        ("Cargo.toml", "[workspace.dependencies]\nserde='1'\n[dependencies]\na={package='axum',version='0.8'}\nprivate={path='vendor/private'}\n[target.'cfg(unix)'.dependencies]\nhttp='=1.3.1'"),
        ("Cargo.lock", "[[package]]\nname='serde'\nversion='1.0.225'\nsource='registry+https://index.crates.io'\n[[package]]\nname='axum'\nversion='0.8.6'\nsource='registry+https://index.crates.io'\n[[package]]\nname='private'\nversion='1.0.0'"),
        ("go.mod", "module example.test/private\nrequire (\n github.com/original/dep v1.2.3\n golang.org/x/text v0.17.0 // indirect\n example.test/private v2.1.0\n)\nreplace github.com/original/dep v1.2.3 => github.com/official/fork v1.2.4\nreplace example.test/private => ../private\n"),
        ("pyproject.toml", "[project]\ndependencies=['My_Package>=2','not-pinned==1.*','direct @ file:../hidden']\n[tool.poetry.group.dev.dependencies]\npytest='^8'"),
        ("uv.lock", "[[package]]\nname='my-package'\nversion='2.3.0'\nsource={registry='https://pypi.org/simple'}\n[[package]]\nname='private'\nversion='1.0.0'\nsource={editable='.'}"),
        ("poetry.lock", "[[package]]\nname='pytest'\nversion='8.4.2'"),
        ("requirements-prod.txt", "Requests[security]==2.32.4 ; python_version > '3.9'\n-e ./hidden\n"),
        ("composer.json", r#"{"require":{"vendor/tool":"^3.0","php":">=8"}}"#),
        ("composer.lock", r#"{"packages":[{"name":"vendor/tool","version":"v3.2.0"}]}"#),
        ("Gemfile.lock", "PATH\n  remote: ../secret\n  specs:\n    private (1.0.0)\nGEM\n  remote: https://rubygems.org/\n  specs:\n    rack (3.1.8)\n      ruby2_keywords (>= 0.0.5)"),
        ("pom.xml", "<project><properties><guava.version>33.4.0-jre</guava.version></properties><dependencies><dependency><groupId>com.google.guava</groupId><artifactId>guava</artifactId><version>${guava.version}</version></dependency></dependencies></project>"),
        ("build.gradle.kts", "dependencies { implementation(\"org.example:core:1.5.0\") }"),
        ("pubspec.yaml", "dependencies:\n  characters: ^1.4.0\n  flutter:\n    sdk: flutter\n"),
        ("pubspec.lock", "packages:\n  characters:\n    source: hosted\n    version: '1.4.1'\n"),
        ("Package.swift", "let packages = [.package(url: \"https://github.com/apple/swift-argument-parser\", from: \"1.0.0\")]"),
        ("Package.resolved", r#"{"pins":[{"identity":"swift-argument-parser","location":"https://github.com/apple/swift-argument-parser.git","state":{"version":"1.6.1"}}]}"#),
        ("app.csproj", "<ItemGroup><PackageReference Include=\"Newtonsoft.Json\" Version=\"13.0.4\"/><PackageReference Include=\"NodaTime\"><Version>3.2.1</Version></PackageReference></ItemGroup>"),
        (".gitignore", "ignored/\n"),
        ("ignored/package.json", r#"{"dependencies":{"private":"1.0.0"}}"#),
        ("node_modules/package.json", r#"{"dependencies":{"private":"1.0.0"}}"#),
    ];
    for (file, text) in cases {
        let path = home.0.join(file);
        fs::create_dir_all(path.parent().ok_or("fixture missing parent")?)?;
        fs::write(path, text)?;
    }
    std::os::unix::fs::symlink(home.0.join("ignored"), home.0.join("linked"))?;
    let results = stack::scan(&home.0);
    for (name, version) in [
        ("react", "19.1.2"),
        ("lodash", "4.17.21"),
        ("pin", "2.0.0"),
        ("@scope/example", "2.1.0"),
        ("better", "2.3.1"),
        ("minar-fixture", "7.5.1"),
        ("axum", "0.8.6"),
        ("serde", "1.0.225"),
        ("http", "1.3.1"),
        ("golang.org/x/text", "v0.17.0"),
        ("github.com/official/fork", "v1.2.4"),
        ("my-package", "2.3.0"),
        ("requests", "2.32.4"),
        ("pytest", "8.4.2"),
        ("rack", "3.1.8"),
        ("vendor/tool", "v3.2.0"),
        ("com.google.guava:guava", "33.4.0-jre"),
        ("org.example:core", "1.5.0"),
        ("characters", "1.4.1"),
        ("apple/swift-argument-parser", "1.6.1"),
        ("newtonsoft.json", "13.0.4"),
        ("nodatime", "3.2.1"),
    ] {
        assert!(
            results
                .iter()
                .any(|r| r.name == name && r.version.as_deref() == Some(version)),
            "missing {name}@{version}: {results:?}"
        );
    }
    assert!(results
        .iter()
        .any(|r| r.name == "not-pinned" && r.version.is_none()));
    assert!(results.iter().all(|r| !r.name.contains("private")
        && r.name != "workspace-secret"
        && r.name != "flutter"
        && r.name != "direct"));
    Ok(())
}

fn app(home: &ProbeHome) -> db::Result<(AppState, Value)> {
    let project = home.0.join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("package.json"),
        r#"{"name":"local-private-project","dependencies":{"react":"^19.1.0"},"repository":"https://private.example/repo"}"#,
    )?;
    fs::write(
        project.join("package-lock.json"),
        r#"{"packages":{"node_modules/react":{"version":"19.1.2"}}}"#,
    )?;
    fs::write(project.join("App.js"), SOURCE_MARKER)?;
    fs::write(project.join(".env"), "DO_NOT_SEND=fixture-local-env")?;
    let config: crate::Config = serde_json::from_value(
        json!({"database":home.0.join("state.sqlite"), "fixedClock":2000000, "testKeyring":{}}),
    )?;
    let database = db::open(&config.database, false, config.now())?;
    crate::initialize_local(&database)?;
    database.connection.execute_batch(&format!(
        "INSERT INTO agents(id,name,model,instructions) VALUES('{AGENT}','Test','','');
         INSERT INTO threads(id,agent,name) VALUES('thread','{AGENT}','Test');
         INSERT INTO local_projects(id,name,description,created,updated,channel) VALUES('project','Test','',0,0,'section');
         INSERT INTO local_project_threads VALUES('thread','project','');
         INSERT INTO local_project_members VALUES('project','{AGENT}',0);
         INSERT INTO turns(id,agent,thread,text,status,created) VALUES('turn','{AGENT}','thread','Test','running','');"
    ))?;
    database.connection.execute(
        "INSERT INTO local_project_folders VALUES('project',?)",
        [project.to_string_lossy().as_ref()],
    )?;
    let state = AppState {
        computer: Arc::new(crate::computer::Manager::new(&config)?),
        database: Arc::new(Mutex::new(database)),
        config: Arc::new(config),
        origin: "http://127.0.0.1:4931".into(),
        auth: None,
        events: crate::events::Hub::new(),
        turns: super::super::Runtime::default(),
        whatsapp: Default::default(),
        trial_pending: Arc::default(),
    };
    let turn = json!({"id":"turn", "agent":AGENT, "thread":"thread"});
    Ok((state, turn))
}

#[derive(Debug)]
struct RequestLog {
    path: String,
    query: Vec<(String, String)>,
    authenticated: bool,
    private_headers: bool,
    body_empty: bool,
}

struct Fixture {
    log: Arc<Mutex<Vec<RequestLog>>>,
    task: tokio::task::JoinHandle<()>,
    sources: Sources,
}

impl Fixture {
    async fn new(
        handler: impl Fn(&Request) -> Response + Clone + Send + Sync + 'static,
    ) -> db::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let mut sources = Sources::default();
        sources.fixture = Some(address);
        sources.policy = super::super::web::FetchPolicy {
            loopback: Some(address),
        };
        let log = Arc::new(Mutex::new(Vec::new()));
        let (log_requests, call) = (log.clone(), handler);
        let router = axum::Router::new().fallback(move |request: Request| {
            let logs = log_requests.clone();
            let call = call.clone();
            async move {
                let response = call(&request);
                let (parts, body) = request.into_parts();
                let body_empty = axum::body::to_bytes(body, 1024)
                    .await
                    .is_ok_and(|b| b.is_empty());
                let fixture_request = RequestLog {
                    path: parts.uri.path().into(),
                    query: url::form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes())
                        .into_owned()
                        .collect(),
                    authenticated: crate::text_header(&parts.headers, "authorization")
                        .is_some_and(|value| value == format!("Bearer {SECRET}")),
                    private_headers: ["cookie", "referer", "x-forwarded-for"]
                        .iter()
                        .any(|name| parts.headers.contains_key(*name)),
                    body_empty,
                };
                if let Ok(mut log) = logs.lock() {
                    log.push(fixture_request);
                }
                response
            }
        });
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self { log, task, sources })
    }

    fn logs(&self) -> db::Result<std::sync::MutexGuard<'_, Vec<RequestLog>>> {
        self.log.lock().map_err(|_| "fixture log poisoned".into())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn local(
    state: &AppState,
    path: &str,
    bearer: Option<&str>,
    value: Option<Value>,
) -> db::Result<(StatusCode, Value)> {
    let mut request = Request::builder()
        .uri(path)
        .header("host", "127.0.0.1:4931");
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let body = match value {
        Some(value) => {
            request = request
                .method("POST")
                .header("content-type", "application/json")
                .header("origin", &state.origin)
                .header("sec-fetch-site", "same-origin");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = crate::dispatch(axum::extract::State(state.clone()), request.body(body)?).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

#[tokio::test]
async fn versioned_lookup_caches_without_sending_workspace_or_secret_content() -> db::Result<()> {
    let home = ProbeHome::new()?;
    let (state, turn) = app(&home)?;
    let fixture = Fixture::new(|request| match request.uri().path() {
        "/context7.com/api/v2/libs/search" => axum::Json(json!({"results":[{"id":"/other/unrelated","title":"Unrelated"},{"id":"/reactjs/react.dev","title":"React","state":"finalized","versions":["v19.1.2"]}]})).into_response(),
        "/context7.com/api/v2/context" if request.headers().contains_key("authorization") => format!("React useEffect cleanup docs\n{SECRET}").into_response(),
        "/context7.com/api/v2/context" => "React useEffect cleanup docs".into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }).await?;
    let args = json!({"library":"react","topic":"useEffect cleanup"});
    let fetch = |args: Value| {
        let (state, turn, sources) = (&state, &turn, &fixture.sources);
        async move {
            run(state, turn, "library_docs", &args, sources)
                .await
                .map_err(|e| e.message)
        }
    };
    let first = fetch(args.clone()).await?;
    assert_eq!(first["source"], "Context7");
    assert_eq!(first["version"], "19.1.2");
    assert_eq!(first["cached"], false);
    assert_eq!(fetch(args.clone()).await?["cached"], true);
    assert!(!fixture.logs()?.iter().any(|entry| entry.authenticated));

    let (status, list) = local(&state, "/api/vault", None, Some(json!({"name":crate::computer::vault::CONTEXT7_KEY, "kind":"secret", "url":"https://context7.com", "value":SECRET, "revision":0, "agents":[]}))).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!list.to_string().contains(SECRET));
    {
        let guard = state.database.lock().map_err(|_| "database lock")?;
        guard
            .connection
            .execute("UPDATE library_docs_cache SET expires=0", [])?;
        let bytes: Vec<u8> =
            guard
                .connection
                .query_row("SELECT payload FROM vault_entries", [], |row| row.get(0))?;
        assert!(!crate::mcp::secrets::reflected(&bytes, SECRET));
    }
    let result = fetch(args).await?;
    assert_eq!(result["cached"], false);
    assert!(!result.to_string().contains(SECRET));
    let bad = json!({"library":"react", "topic":SECRET});
    assert!(fetch(bad).await.is_err());
    let code = json!({"library":"react", "topic":"function MyApp() { return secret; }"});
    assert!(fetch(code).await.is_err());
    let extra = json!({"library":"react", "topic":"hooks", "source":SOURCE_MARKER});
    assert!(fetch(extra).await.is_err());
    let logs = fixture.logs()?;
    assert_eq!(logs.len(), 4);
    assert!(logs
        .iter()
        .all(|row| !row.private_headers && row.body_empty));
    for index in [0, 2] {
        assert_eq!(
            logs[index].query,
            [
                ("libraryName".into(), "react".into()),
                ("query".into(), "useEffect cleanup".into())
            ]
        );
        assert_eq!(
            logs[index + 1].query,
            [
                ("libraryId".into(), "/reactjs/react.dev".into()),
                ("query".into(), "useEffect cleanup".into()),
                ("type".into(), "txt".into()),
                ("version".into(), "v19.1.2".into())
            ]
        );
    }
    assert!(logs[2..].iter().all(|entry| entry.authenticated));
    Ok(())
}

#[tokio::test]
async fn context7_failure_and_retry_after_fall_through_to_official_sources() -> db::Result<()> {
    let home = ProbeHome::new()?;
    let (state, turn) = app(&home)?;
    let fixture = Fixture::new(|request| {
        let path = request.uri().path();
        match path {
            "/context7.com/api/v2/libs/search" => {
                if request.uri().query().is_some_and(|q| q.contains("libraryName=react")) {
                    return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "120")]).into_response();
                }
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            "/registry.npmjs.org/react/19.1.2" => return axum::Json(json!({"version":"19.1.2","description":"npm metadata"})).into_response(),
            "/pypi.org/pypi/requests/2.32.4/json" => return axum::Json(json!({"info":{"version":"2.32.4","description":"PyPI readme"}})).into_response(),
            "/rubygems.org/api/v2/rubygems/rack/versions/3.1.8.json" => return axum::Json(json!({"number":"3.1.8","description":"RubyGems readme"})).into_response(),
            "/repo.packagist.org/p2/vendor/tool.json" => return axum::Json(json!({"packages":{"vendor/tool":[{"version":"v4.0.0","description":"wrong version"},{"version":"v3.2.0","description":"Packagist readme"}]}})).into_response(),
            "/docs.rs/serde/1.0.225" => return "docs.rs crate reference".into_response(),
            "/pkg.go.dev/golang.org/x/text@v0.17.0" => return "pkg.go.dev library reference".into_response(),
            "/javadoc.io/doc/org.example/api/2.0.0" => return "Java API reference".into_response(),
            "/pub.dev/api/packages/characters/versions/1.4.1" => return axum::Json(json!({"version":"1.4.1","pubspec":{"description":"Dart character API"}})).into_response(),
            "/swiftpackageindex.com/apple/swift-argument-parser" => return "Swift ArgumentParser".into_response(),
            "/www.nuget.org/packages/nodatime/3.2.1" => return "NuGet API reference".into_response(),
            "/registry.npmjs.org/tool/latest" => return axum::Json(json!({"homepage":"https://docs.example.test/guide?tracking=ignore", "description":"npm summary"})).into_response(),
            "/docs.example.test/llms.txt" => return "# Tool\n\nIndex".into_response(),
            "/docs.example.test/llms-full.txt" => return "# Tool\n\nuseThing cleanup guide".into_response(),
            "/registry.npmjs.org/other/latest" => return axum::Json(json!({"homepage":"https://site.example.test/docs"})).into_response(),
            "/site.example.test/docs" => return axum::response::Html("<h1>Official manual</h1><p>Install other</p>").into_response(),
            _ => StatusCode::NOT_FOUND,
        }.into_response()
    }).await?;
    let lookup = |name: &str, version: &str, ecosystem| Lookup {
        name: name.into(),
        topic: "useThing cleanup".into(),
        version: (!version.is_empty()).then(|| version.into()),
        ecosystem: Some(ecosystem),
    };
    use Ecosystem::*;
    let first = run(
        &state,
        &turn,
        "library_docs",
        &json!({"library":"react","topic":"useEffect cleanup"}),
        &fixture.sources,
    )
    .await
    .map_err(|e| e.message)?;
    assert_eq!(first["source"], "npm");
    assert_eq!(first["source_version"], "19.1.2");
    let request = lookup("react", "19.1.2", Npm);
    assert!(fixture
        .sources
        .context7(&request, None, 2_119_999)
        .await
        .is_none());
    let cases = [
        ("react", "19.1.2", Npm, "npm"),
        ("serde", "1.0.225", Rust, "docs.rs"),
        ("requests", "2.32.4", Python, "PyPI"),
        ("rack", "3.1.8", Ruby, "RubyGems"),
        ("vendor/tool", "v3.2.0", Php, "Packagist"),
        ("golang.org/x/text", "v0.17.0", Go, "pkg.go.dev"),
        ("org.example:api", "2.0.0", Maven, "javadoc.io"),
        ("characters", "1.4.1", Dart, "pub.dev"),
        (
            "apple/swift-argument-parser",
            "",
            Swift,
            "Swift Package Index",
        ),
        ("nodatime", "3.2.1", Nuget, "NuGet"),
        ("tool", "", Npm, "llms-full.txt"),
        ("other", "", Npm, "Official documentation"),
    ];
    for (name, version, ecosystem, source) in cases {
        let docs = fixture
            .sources
            .official(&lookup(name, version, ecosystem))
            .await
            .ok_or_else(|| format!("missing fallback {source}"))?;
        assert_eq!(docs.source, source);
        assert!(!docs.content.contains("wrong version"));
        if !version.is_empty() {
            assert_eq!(docs.source_version.as_deref(), Some(version));
        }
    }
    assert!(fixture
        .sources
        .context7(&lookup("tool", "", Npm), None, 2_120_000)
        .await
        .is_none());
    let logs = fixture.logs()?;
    assert_eq!(
        logs.iter()
            .filter(|request| request.path.starts_with("/context7.com/"))
            .count(),
        2
    );
    assert!(logs
        .iter()
        .skip(1)
        .all(|row| !row.authenticated && row.query.iter().all(|(name, _)| name != "tracking")));
    Ok(())
}

#[tokio::test]
async fn api_and_mcp_follow_web_access_and_ignore_the_old_docs_flag() -> db::Result<()> {
    let home = ProbeHome::new()?;
    let (state, turn) = app(&home)?;
    assert!(summary(&state, &turn)
        .await
        .map_err(|e| e.message)?
        .contains("react@19.1.2"));
    let (_, token) = crate::mcp::mint_session(&state, AGENT, "turn").map_err(|e| e.message)?;
    let api_stack = super::super::tools::run(
        &state,
        &turn,
        &json!({"id":"api-stack","function":{"name":"project_stack","arguments":"{}"}}),
    )
    .await
    .map_err(|e| e.message)?;
    assert_eq!(api_stack["libraries"][0]["version"], "19.1.2");
    let rpc = |method, params| {
        local(
            &state,
            "/mcp",
            Some(&token),
            Some(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})),
        )
    };
    let (status, list) = rpc("tools/list", json!({})).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(
        list.to_string().contains("project_stack") && list.to_string().contains("library_docs")
    );
    let call = json!({"name":"project_stack","arguments":{}});
    let (status, result) = rpc("tools/call", call.clone()).await?;
    assert_eq!(status, StatusCode::OK);
    let text = result["result"]["content"][0]["text"]
        .as_str()
        .ok_or("missing MCP docs response")?;
    let stack: Value = serde_json::from_str(text)?;
    assert_eq!(
        stack["libraries"][0],
        json!({"name":"react","version":"19.1.2","ecosystem":"npm"})
    );
    {
        let guard = state.database.lock().map_err(|_| "database lock")?;
        guard.connection.execute(
            "UPDATE agents SET library_docs_access=0 WHERE id=?",
            [AGENT],
        )?;
    }
    let (_, list) = rpc("tools/list", json!({})).await?;
    assert!(
        list.to_string().contains("project_stack") && list.to_string().contains("library_docs")
    );
    assert_eq!(rpc("tools/call", call.clone()).await?.0, StatusCode::OK);
    {
        let guard = state.database.lock().map_err(|_| "database lock")?;
        guard
            .connection
            .execute("UPDATE agents SET web_access=0 WHERE id=?", [AGENT])?;
    }
    let (_, list) = rpc("tools/list", json!({})).await?;
    assert!(
        !list.to_string().contains("project_stack") && !list.to_string().contains("library_docs")
    );
    assert!(summary(&state, &turn)
        .await
        .map_err(|e| e.message)?
        .is_empty());
    assert!(tool(&state, &turn, "project_stack", &json!({}))
        .await
        .is_err());
    let (status, result) = rpc("tools/call", call).await?;
    assert!(status.is_client_error() || result.get("error").is_some());
    Ok(())
}

#[test]
fn existing_agents_migrate_old_docs_and_craft_access() -> db::Result<()> {
    let home = ProbeHome::new()?;
    let (state, _) = app(&home)?;
    let database = state.database.lock().map_err(|_| "database lock")?;
    let db = &database.connection;
    db.execute_batch(
        "CREATE TABLE craft_grants(agent TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE) STRICT;",
    )?;
    db.execute(
        "UPDATE agents SET library_docs_access=0 WHERE id=?",
        [AGENT],
    )?;
    db.execute("UPDATE local_projects SET codegraph_enabled=0", [])?;
    db.execute(
        "INSERT INTO agent_edits VALUES('legacy-edit',?,'{\"libraryDocsAccess\":false}','{\"library_docs_access\":0}')",
        [AGENT],
    )?;
    crate::data::initialize(&database)?;
    assert!(enabled(db, AGENT).map_err(|error| error.message)?);
    assert_eq!(
        db.query_row(
            "SELECT library_docs_access FROM agents WHERE id=?",
            [AGENT],
            |row| row.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(
        db.query_row("SELECT codegraph_enabled FROM local_projects", [], |row| {
            row.get::<_, i64>(0)
        })?,
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT request,result FROM agent_edits WHERE id='legacy-edit'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        )?,
        ("{}".into(), "{\"library_docs_access\":1}".into())
    );
    assert!(!crate::data::table(db, "craft_grants").map_err(|error| error.message)?);
    Ok(())
}
