use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    routing::get,
    Router,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

const ORIGIN: &str = "http://127.0.0.1:4418";
const KEY: &str = "fixture-provider-secret";
const REPLACEMENT: &str = "fixture-replacement-secret";
const ENV_KEY: &str = "fixture-environment-secret";

struct Fixture {
    directory: PathBuf,
    state: AppState,
    base: String,
    calls: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> crate::db::Result<Self> {
        Self::with_environment_accounts(true).await
    }

    async fn with_environment_accounts(configured: bool) -> crate::db::Result<Self> {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let router = Router::new().route(
            "/v1/models",
            get(move |request: Request| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    let key = crate::text_header(request.headers(), "authorization")
                        .and_then(|header| header.strip_prefix("Bearer "))
                        .unwrap_or("");
                    if [KEY, REPLACEMENT].contains(&key) {
                        json_response(200, json!({"data":[{"id":"model-a"},{"id":"model-b"}]}))
                    } else {
                        json_response(401, json!({"error":key}))
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            // A failed fixture server surfaces as failed requests in the test.
            let _ = axum::serve(listener, router).await;
        });
        let directory =
            std::env::temp_dir().join(format!("minarbot-provider-test-{}", uuid::Uuid::new_v4()));
        let mut options = json!({
            "database": directory.join("minarbot.sqlite"),
            "harness": {"executables": {"codex": std::env::current_exe()?, "claude": std::env::current_exe()?}},
        });
        if configured {
            options["baseURL"] = json!(base);
            options["apiKey"] = json!(ENV_KEY);
            options["defaultModel"] = json!("env-model");
            options["accounts"] =
                json!([{"id":"env", "name":"Environment", "baseURL":base, "apiKey":ENV_KEY}]);
        }
        let mut config: crate::config::Config = serde_json::from_value(options)?;
        config.data.validate()?;
        let database = crate::db::open(&config.database, false, 0)?;
        crate::data::initialize(&database)?;
        crate::files::init(&database.connection)?;
        crate::turns::initialize(&database.connection).map_err(|error| error.message)?;
        crate::connectors::initialize(&database.connection).map_err(|error| error.message)?;
        let computer = Arc::new(crate::computer::Manager::new(&config)?);
        let state = AppState {
            database: Arc::new(Mutex::new(database)),
            config: Arc::new(config),
            origin: ORIGIN.into(),
            auth: None,
            events: crate::events::Hub::new(),
            turns: crate::turns::Runtime::default(),
            computer,
            whatsapp: Default::default(),
            trial_pending: Arc::new(AtomicBool::new(false)),
        };
        Ok(Self {
            state,
            directory,
            base,
            calls,
            server,
        })
    }

    async fn request(
        &self,
        path: &str,
        input: Option<Value>,
        origin: &str,
    ) -> crate::db::Result<(StatusCode, Value)> {
        let mut request = Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:4418")
            .header("origin", origin);
        let body = if let Some(input) = input {
            request = request
                .method(Method::POST)
                .header("content-type", "application/json");
            Body::from(input.to_string())
        } else {
            Body::empty()
        };
        let response = crate::dispatch(State(self.state.clone()), request.body(body)?).await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64000).await?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let text = value.to_string();
        assert!(![KEY, REPLACEMENT, ENV_KEY, "invalid-provider-secret"]
            .iter()
            .any(|key| text.contains(key)));
        assert!(!text.contains("apiKey\":") && !text.contains("key_ciphertext"));
        Ok((status, value))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tokio::test]
async fn device_host_resolves_settings_account_without_advertising_key() -> crate::db::Result<()> {
    let fixture = Fixture::new().await?;
    let account = id();
    let body = json!({"id":account,"name":"Device account","provider":"openai-compatible",
        "baseURL":fixture.base,"apiKey":KEY,"defaultModel":"model-a"});
    let (status, _) = fixture
        .request("/api/provider-accounts", Some(body), ORIGIN)
        .await?;
    assert_eq!(status, 201);
    crate::computer::devices::host::check_settings_account(
        fixture.state.config.data.clone(),
        fixture.state.config.database.clone(),
        &account,
        &fixture.base,
        KEY,
    )
}

#[tokio::test]
async fn agent_directory_checks_each_runtime_and_bound_account() -> crate::db::Result<()> {
    let fixture = Fixture::with_environment_accounts(false).await?;
    let agent = id();
    let profile = json!({
        "id": agent, "name": "Coder", "instructions": "Answer", "model": "model-a",
        "runtime": "codex",
    });
    assert_eq!(
        fixture
            .request("/api/agents", Some(profile.clone()), ORIGIN)
            .await?
            .0,
        201
    );
    let (_, directory) = fixture.request("/api/agents", None, ORIGIN).await?;
    assert_eq!(directory["configured"], false);
    assert_eq!(directory["agents"][0]["runnable"], false);
    assert_eq!(
        directory["agents"][0]["blocked_reason"],
        "Codex needs an OpenAI-compatible provider."
    );
    assert_eq!(
        fixture
            .request(
                &format!("/api/agents/{agent}/turns"),
                Some(json!({"id":id(),"text":"Answer"})),
                ORIGIN,
            )
            .await?
            .0,
        503
    );

    let account = id();
    assert_eq!(
        fixture
            .request(
                "/api/provider-accounts",
                Some(json!({
                    "id": account, "name": "Router", "provider": "openai-compatible",
                    "baseURL": fixture.base, "apiKey": KEY, "defaultModel": "model-a",
                })),
                ORIGIN
            )
            .await?
            .0,
        201
    );
    let (_, directory) = fixture.request("/api/agents", None, ORIGIN).await?;
    assert_eq!(directory["configured"], false);
    let mut profile = profile;
    profile["id"] = json!(id());
    profile["runtime"] = json!("api");
    profile["providerAccount"] = json!(account);
    assert_eq!(
        fixture
            .request("/api/agents", Some(profile), ORIGIN)
            .await?
            .0,
        201
    );
    let (_, directory) = fixture.request("/api/agents", None, ORIGIN).await?;
    assert_eq!(directory["configured"], true);
    assert_eq!(directory["agents"][0]["runnable"], false);
    assert_eq!(directory["agents"][1]["runnable"], true);
    assert_eq!(directory["agents"][1]["blocked_reason"], Value::Null);
    Ok(())
}

#[tokio::test]
async fn engine_login_profiles_check_and_guard_agent_bindings() -> crate::db::Result<()> {
    let mut fixture = Fixture::with_environment_accounts(false).await?;
    let executable = fixture.directory.join("fake-engine");
    fs::write(
        &executable,
        r#"#!/bin/sh
case "$*" in
  "login status") test "$HOME" = "$CODEX_HOME" && test -f "$CODEX_HOME/auth.json" ;;
  "auth status --json") test "$HOME" = "$CLAUDE_CONFIG_DIR" && test -f "$CLAUDE_CONFIG_DIR/.credentials.json" ;;
  *) exit 2 ;;
esac
"#,
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    Arc::get_mut(&mut fixture.state.config)
        .ok_or("Fixture config is shared")?
        .harness
        .harness = Some(json!({"executables":{"codex":executable,"claude":executable}}));

    for (runtime, other, file, variable, command) in [
        ("codex", "claude", "auth.json", "CODEX_HOME", "codex login"),
        (
            "claude",
            "codex",
            ".credentials.json",
            "CLAUDE_CONFIG_DIR",
            "claude auth login",
        ),
    ] {
        let login = id();
        let path = fixture
            .directory
            .join("engine-logins")
            .join(runtime)
            .join(&login);
        let add = json!({"id":login,"runtime":runtime,"name":"Personal"});
        let check = format!("/api/engine-logins/{login}/check");
        let remove = format!("/api/engine-logins/{login}/remove");
        let action = || Some(json!({"id":id()}));
        assert_eq!(
            fixture
                .request("/api/engine-logins", Some(add.clone()), "http://other.test")
                .await?
                .0,
            403
        );
        let (status, created) = fixture
            .request("/api/engine-logins", Some(add.clone()), ORIGIN)
            .await?;
        assert_eq!(status, 201);
        assert_eq!(
            created["command"],
            format!("{variable}='{}' {command}", path.display())
        );
        assert_eq!(created["signedIn"], false);
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fixture
                .request("/api/engine-logins", Some(add), ORIGIN)
                .await?
                .0,
            200
        );
        let (_, checked) = fixture.request(&check, action(), ORIGIN).await?;
        assert!(checked["checkedAt"].is_string());
        assert_eq!(checked["signedIn"], false);

        let agent = id();
        let mut profile = json!({
            "id":agent,"runtime":other,"providerAccount":format!("login:{login}"),
            "name":"Coder","instructions":"Answer","model":"default"
        });
        assert_eq!(
            fixture
                .request("/api/agents", Some(profile.clone()), ORIGIN)
                .await?
                .0,
            400
        );
        profile["runtime"] = json!(runtime);
        assert_eq!(
            fixture
                .request("/api/agents", Some(profile.clone()), ORIGIN)
                .await?
                .0,
            201
        );
        let current = |directory: Value| -> crate::db::Result<Value> {
            directory["agents"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == agent))
                .cloned()
                .ok_or_else(|| "Agent missing from fixture directory".into())
        };
        assert_eq!(
            current(fixture.request("/api/agents", None, ORIGIN).await?.1)?["runnable"],
            false
        );
        let turns = format!("/api/agents/{agent}/turns");
        assert_eq!(
            fixture
                .request(&turns, Some(json!({"id":id(),"text":"Hello"})), ORIGIN)
                .await?
                .0,
            503
        );

        let fake_host = fixture.directory.join("fake-host-auth");
        fs::write(&fake_host, b"outside-profile-sentinel")?;
        let credential = path.join(file);
        std::os::unix::fs::symlink(&fake_host, &credential)?;
        assert_eq!(fixture.request(&check, action(), ORIGIN).await?.0, 409);
        fs::remove_file(&credential)?;
        fs::hard_link(&fake_host, &credential)?;
        assert_eq!(fixture.request(&check, action(), ORIGIN).await?.0, 409);
        fs::remove_file(&credential)?;
        fs::write(&credential, b"fake-login-credential")?;
        let (status, checked) = fixture.request(&check, action(), ORIGIN).await?;
        assert_eq!(status, 200);
        assert_eq!(checked["signedIn"], true);
        assert!(!checked.to_string().contains("fake-login-credential"));
        assert_eq!(
            current(fixture.request("/api/agents", None, ORIGIN).await?.1)?["runnable"],
            true
        );

        assert_eq!(fixture.request(&remove, action(), ORIGIN).await?.0, 409);
        let edit = format!("/api/agents/{agent}/edit");
        profile["id"] = json!(id());
        profile["revision"] = json!(1);
        profile["runtime"] = json!("api");
        profile["confirmRuntime"] = json!(true);
        assert_eq!(
            fixture
                .request(&edit, Some(profile.clone()), ORIGIN)
                .await?
                .0,
            400
        );
        profile["providerAccount"] = json!("");
        assert_eq!(fixture.request(&edit, Some(profile), ORIGIN).await?.0, 200);
        assert_eq!(fixture.request(&remove, action(), ORIGIN).await?.0, 200);
        assert!(!path.exists());
        assert_eq!(fixture.request(&check, action(), ORIGIN).await?.0, 404);
        assert_eq!(
            fixture.request("/api/engine-logins", None, ORIGIN).await?.1,
            json!([])
        );
        assert_eq!(fs::read(&fake_host)?, b"outside-profile-sentinel");
    }
    Ok(())
}

#[tokio::test]
async fn settings_routes_verify_and_apply_accounts_live() -> crate::db::Result<()> {
    let fixture = Fixture::new().await?;
    let path = "/api/provider-accounts";
    let account = id();
    let payload = json!({"id":account,"name":"Router","provider":"openai-compatible","baseURL":fixture.base,"apiKey":KEY,"defaultModel":"model-a"});
    let (_, accounts) = fixture.request(path, None, ORIGIN).await?;
    assert_eq!(accounts.as_array().map(Vec::len), Some(2));
    assert!(accounts.as_array().is_some_and(|rows| rows
        .iter()
        .all(|row| row["source"] == "environment" && row["hasKey"] == true)));

    let (status, _) = fixture
        .request(path, Some(payload.clone()), "https://foreign.example")
        .await?;
    assert_eq!(status, 403);
    assert_eq!(fixture.calls.load(Ordering::Relaxed), 0);

    let mut invalid = payload.clone();
    invalid["apiKey"] = json!("invalid-provider-secret");
    let (status, body) = fixture.request(path, Some(invalid), ORIGIN).await?;
    assert_eq!(status, 502);
    assert!(string(&body, "error").contains("rejected the API key"));
    let (_, accounts) = fixture.request(path, None, ORIGIN).await?;
    assert_eq!(accounts.as_array().map(Vec::len), Some(2));
    assert!(!fixture
        .directory
        .join("minarbot.sqlite.provider-key")
        .exists());

    let (status, body) = fixture
        .request(&format!("{path}/test"), Some(payload.clone()), ORIGIN)
        .await?;
    assert_eq!(status, 200);
    assert_eq!(body["models"], json!(["model-a", "model-b"]));
    assert_eq!(fixture.calls.load(Ordering::Relaxed), 2);

    let (status, body) = fixture.request(path, Some(payload), ORIGIN).await?;
    assert_eq!(status, 201);
    assert_eq!(body["id"], account);
    assert_eq!(body["hasKey"], true);
    assert_eq!(body["source"], "settings");
    assert_eq!(body["revision"], 1);
    assert_eq!(fixture.calls.load(Ordering::Relaxed), 3);
    let key_path = fixture.directory.join("minarbot.sqlite.provider-key");
    assert_eq!(fs::metadata(&key_path)?.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(key_path)?.len(), 32);
    for name in ["minarbot.sqlite", "minarbot.sqlite-wal"] {
        assert!(!fs::read(fixture.directory.join(name))?
            .windows(KEY.len())
            .any(|window| window == KEY.as_bytes()));
    }
    let (status, models) = fixture
        .request(&format!("/api/models?account={account}"), None, ORIGIN)
        .await?;
    assert_eq!(status, 200);
    assert_eq!(models["models"], json!(["model-a", "model-b"]));

    let (status, _) = fixture
        .request(
            &format!("{path}/{account}/test"),
            Some(json!({"id":id()})),
            ORIGIN,
        )
        .await?;
    assert_eq!(status, 200);
    let edit = format!("{path}/{account}/edit");
    let change =
        json!({"id":id(),"revision":1,"name":"Renamed","defaultModel":"model-b","apiKey":""});
    let (status, body) = fixture.request(&edit, Some(change.clone()), ORIGIN).await?;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "Renamed");
    assert_eq!(body["defaultModel"], "model-b");
    let (status, _) = fixture.request(&edit, Some(change), ORIGIN).await?;
    assert_eq!(status, 409);

    let (status, body) = fixture
        .request(
            &edit,
            Some(json!({"id":id(),"revision":2,"baseURL":"https://another.example/v1"})),
            ORIGIN,
        )
        .await?;
    assert_eq!(status, 400);
    assert!(string(&body, "error").contains("apiKey"));
    let (status, _) = fixture
        .request(
            &edit,
            Some(json!({"id":id(),"revision":2,"apiKey":"invalid-provider-secret"})),
            ORIGIN,
        )
        .await?;
    assert_eq!(status, 502);
    let (status, _) = fixture
        .request(
            &edit,
            Some(json!({"id":id(),"revision":2,"apiKey":REPLACEMENT})),
            ORIGIN,
        )
        .await?;
    assert_eq!(status, 200);
    let live = providers::accounts(&fixture.state).map_err(|error| error.message)?;
    assert!(live
        .iter()
        .any(|row| row.id == account && row.key.as_deref() == Some(REPLACEMENT)));

    let agent = id();
    let (status, _) = fixture.request("/api/agents", Some(json!({
        "id":agent, "name":"Uses router", "model":"model-b", "instructions":"Answer", "providerAccount":account,
    })), ORIGIN).await?;
    assert_eq!(status, 201);
    let remove = format!("{path}/{account}/delete");
    let (status, body) = fixture
        .request(&remove, Some(json!({"id":id()})), ORIGIN)
        .await?;
    assert_eq!(status, 409);
    assert!(string(&body, "error").contains("Uses router"));
    let (status, _) = fixture
        .request(
            &format!("{path}/env/delete"),
            Some(json!({"id":id()})),
            ORIGIN,
        )
        .await?;
    assert_eq!(status, 409);
    {
        let db = fixture
            .state
            .database
            .lock()
            .map_err(|_| "database lock poisoned")?;
        db.connection
            .execute("UPDATE agents SET provider_account='' WHERE id=?", [&agent])?;
    }
    let (status, _) = fixture
        .request(&remove, Some(json!({"id":id()})), ORIGIN)
        .await?;
    assert_eq!(status, 200);
    let (_, accounts) = fixture.request(path, None, ORIGIN).await?;
    assert_eq!(accounts.as_array().map(Vec::len), Some(2));
    Ok(())
}

#[tokio::test]
async fn provider_instances_store_secret_environment_and_redact_it() -> crate::db::Result<()> {
    let fixture = Fixture::with_environment_accounts(false).await?;
    let request = id();
    let create = json!({
        "id":request,
        "instanceId":"codex-secondary",
        "kind":"codex",
        "displayName":"Secondary Codex",
        "enabled":true,
        "environment":[{"name":"ROUTER_KEY","value":"instance-secret","secret":true}],
        "launchArgs":["app-server"]
    });
    let (status, created) = fixture
        .request("/api/provider-instances", Some(create), ORIGIN)
        .await?;
    assert_eq!(status, 201);
    assert_eq!(created["id"], "codex-secondary");
    assert_eq!(created["environment"][0]["value"], "");
    assert_eq!(created["environment"][0]["valueRedacted"], true);
    assert!(!created.to_string().contains("instance-secret"));

    let (_, listed) = fixture
        .request("/api/provider-instances", None, ORIGIN)
        .await?;
    assert!(listed["providers"]
        .as_array()
        .is_some_and(|providers| providers
            .iter()
            .any(|provider| provider["id"] == "codex-secondary")));
    Ok(())
}
