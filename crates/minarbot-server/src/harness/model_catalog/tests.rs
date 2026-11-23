use super::*;
use std::{cell::Cell, fs};

fn fixture(home: &ProbeHome, script: &str) -> Result<Process> {
    Process::launch(
        Path::new("/bin/sh"),
        &["-c".into(), script.into()],
        &home.0,
        &HashMap::new(),
    )
}

#[tokio::test]
async fn codex_follows_the_cursor_filters_hidden_and_marks_default() -> Result<()> {
    let home = ProbeHome::new().map_err(|_| Failure::storage())?;
    let process = fixture(
        &home,
        r#"
        IFS= read -r request
        case "$request" in *'"method":"initialize"'*) ;; *) exit 1 ;; esac
        printf '%s\n' '{"id":1,"result":{}}'
        IFS= read -r request
        case "$request" in *'"method":"initialized"'*) ;; *) exit 1 ;; esac
        IFS= read -r request
        case "$request" in *'"method":"model/list"'*) ;; *) exit 1 ;; esac
        printf '%s\n' '{"id":2,"result":{"data":[{"id":"hidden","hidden":true},{"id":"sol","displayName":"Sol","isDefault":true}],"nextCursor":"page 2"}}'
        IFS= read -r request
        case "$request" in *'"cursor":"page 2"'*) ;; *) exit 1 ;; esac
        printf '%s\n' '{"id":3,"result":{"data":[{"id":"terra"}],"nextCursor":null}}'
    "#,
    )?;
    assert_eq!(
        response(probe_process(process, "codex", PROBE_TIMEOUT).await?),
        json!({"models":["sol","terra"],"catalog":[{"id":"sol","label":"Sol","default":true},{"id":"terra","label":"terra"}]})
    );
    Ok(())
}

#[tokio::test]
async fn codex_caps_paginated_catalogs() -> Result<()> {
    let home = ProbeHome::new().map_err(|_| Failure::storage())?;
    let process = fixture(
        &home,
        r#"
        read -r init
        printf '%s\n' '{"id":1,"result":{}}'
        read -r initialized
        id=2
        while read -r page; do
            printf '{"id":%d,"result":{"data":[],"nextCursor":"cursor-%d"}}\n' "$id" "$id"
            id=$((id + 1))
        done
    "#,
    )?;
    let result = probe_process(process, "codex", PROBE_TIMEOUT).await;
    assert!(
        matches!(result, Err(error) if error.status == 502 && error.message.contains("pagination limit"))
    );
    Ok(())
}

#[tokio::test]
async fn claude_reads_initialize_catalog_after_stdin_closes() -> Result<()> {
    let home = ProbeHome::new().map_err(|_| Failure::storage())?;
    let process = fixture(
        &home,
        r#"
        IFS= read -r request
        case "$request" in *'"subtype":"initialize"'*) ;; *) exit 1 ;; esac
        if read -r extra; then exit 1; fi
        printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init","response":{"models":[{"value":"default","displayName":"Default (recommended)"},{"value":"opus","displayName":"Opus"}]}}}'
    "#,
    )?;
    assert_eq!(
        response(probe_process(process, "claude", PROBE_TIMEOUT).await?),
        json!({"models":["default","opus"],"catalog":[{"id":"default","label":"Default (recommended)","default":true},{"id":"opus","label":"Opus"}]})
    );
    Ok(())
}

#[test]
fn codex_config_reads_provider_names_urls_and_optional_bearer() -> Result<()> {
    let config = br#"
        model_provider = "router"
        [model_providers.router]
        base_url = "https://router.example/v1/"
        env_key = "ROUTER_KEY"
        wire_api = "responses"
        [model_providers."custom.local"]
        base_url = "http://127.0.0.1:3500/v1"
        [model_providers.no_url]
        name = "Missing URL"
    "#;
    let accounts = codex_providers(config, |key| {
        (key == "ROUTER_KEY").then(|| "fixture-key".into())
    });
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].0, "router");
    assert_eq!(accounts[0].1.base, "https://router.example/v1/");
    assert_eq!(accounts[0].1.key.as_deref(), Some("fixture-key"));
    assert_eq!(accounts[1].0, "custom.local");
    assert_eq!(accounts[1].1.key, None);
    assert!(codex_providers(b"invalid TOML", |_| None).is_empty());
    Ok(())
}

#[test]
fn first_party_defaults_follow_the_binding() -> Result<()> {
    let mut account = providers::Account {
        id: "account".into(),
        name: "Account".into(),
        family: "anthropic".into(),
        base: "https://api.anthropic.com/v1".into(),
        key: None,
        model: "main".into(),
        stream: false,
        termination: String::new(),
        max_tokens: 4096,
        source: "fixture",
    };
    let api = api_memory_default(&account).ok_or_else(Failure::storage)?;
    assert_eq!(api, "claude-haiku-5-5");
    assert_eq!(native_memory_default("claude", &[]), Some("haiku"));
    account.family = "openai-compatible".into();
    account.base = "https://api.openai.com/v1".into();
    assert_eq!(api_memory_default(&account), Some("gpt-5.4-mini"));
    account.base = "https://api.openai.com.example.test/v1".into();
    assert_eq!(api_memory_default(&account), None);

    let mut foreign = Model::new("gpt-5.4-mini", "Group model", false);
    foreign.group = Some("other-account".into());
    assert_eq!(native_memory_default("codex", &[foreign]), None);
    let models = [
        Model::new("gpt-5.1-codex-mini", "Older mini", false),
        Model::new("gpt-5.4-mini", "Current mini", false),
    ];
    assert_eq!(
        native_memory_default("codex", &models),
        Some("gpt-5.4-mini")
    );
    assert_eq!(
        native_memory_default("codex", &models[..1]),
        Some("gpt-5.1-codex-mini")
    );
    Ok(())
}

#[test]
fn custom_catalog_requires_prices_text_and_context() -> Result<()> {
    let models = [
        ("main", json!({"context_length":128_000,"pricing":{"prompt":"0.01","completion":"0.02"}})),
        ("budget", json!({"context_length":65_536,"pricing":{"prompt":0.001,"completion":0.002}})),
        ("cheap-short", json!({"context_length":32_768,"pricing":{"prompt":0,"completion":0}})),
        ("cheap-image", json!({"context_length":128_000,"pricing":{"prompt":0,"completion":0},"architecture":{"output_modalities":["image"]}})),
        ("unpriced", json!({"context_length":128_000})),
        ("bad-price", json!({"context_length":128_000,"pricing":{"prompt":"NaN","completion":0}})),
    ].into_iter().map(|(id, row)| (id.to_owned(), MemoryPricing::from_row(&row))).collect();
    let selection = custom_memory_default(&models).ok_or_else(Failure::storage)?;
    let catalog = json!({"memoryDefault": selection});
    assert_eq!(memory_default(&catalog, "main").as_deref(), Some("budget"));
    for main in ["budget", "cheap-short", "unpriced", "bad-price"] {
        assert_eq!(memory_default(&catalog, main), None, "{main}");
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn cache_coalesces_probes_expires_results_and_retries_failures() -> Result<()> {
    let cache = Cache::default();
    let calls = Cell::new(0);
    let probe = || async {
        calls.set(calls.get() + 1);
        tokio::task::yield_now().await;
        Ok(json!({"models":["fixture"]}))
    };
    let (first, second) = tokio::join!(
        cache.get("codex", "device", probe()),
        cache.get("codex", "device", probe()),
    );
    assert_eq!(first?, second?);
    assert_eq!(calls.get(), 1);
    cache.get("codex", "device", probe()).await?;
    assert_eq!(calls.get(), 1);
    tokio::time::advance(CACHE_AGE).await;
    cache.get("codex", "device", probe()).await?;
    assert_eq!(calls.get(), 2);
    assert!(cache
        .get("claude", "device", async { Err(launch_error("claude")) })
        .await
        .is_err());
    cache.get("claude", "device", probe()).await?;
    assert_eq!(calls.get(), 3);
    Ok(())
}

#[tokio::test]
async fn timeout_kills_the_probe() -> crate::db::Result<()> {
    let home = ProbeHome::new()?;
    let process = fixture(&home, r#"printf '%s' "$$" > pid; exec /bin/sleep 60"#)
        .map_err(|error| error.message)?;
    let (result, pid) = tokio::join!(
        probe_process(process, "claude", Duration::from_millis(500)),
        timeout(Duration::from_secs(2), async {
            loop {
                if let Some(pid) = fs::read_to_string(home.0.join("pid"))
                    .ok()
                    .and_then(|pid| pid.parse::<u32>().ok())
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }),
    );
    assert!(
        matches!(result, Err(error) if error.status == 502 && error.message.contains("timed out"))
    );
    let pid = pid?;
    timeout(Duration::from_secs(2), async {
        while Path::new(&format!("/proc/{pid}")).exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    Ok(())
}
