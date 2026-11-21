use super::*;
use crate::db::Result as TestResult;
use std::{fs, io::Write, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

fn document(text: &str, pages: usize) -> TestResult<Vec<u8>> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!(
            "<< /Type /Pages /Count {pages} /Kids [{}] >>",
            (5..pages + 5)
                .map(|id| format!("{id} 0 R"))
                .collect::<Vec<_>>()
                .join(" ")
        )
        .into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    write!(encoder, "BT /F1 12 Tf 40 9900 Td ({text}) Tj ET")?;
    let content = encoder.finish()?;
    let mut object = format!(
        "<< /Length {} /Filter /FlateDecode >>\nstream\n",
        content.len()
    )
    .into_bytes();
    object.extend(content);
    object.extend(b"\nendstream");
    objects.push(object);
    objects.extend((0..pages).map(|_| b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10000 10000] /Resources << /Font << /F1 3 0 R >> >> /Contents 4 0 R >>".to_vec()));
    let mut offsets = vec![];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        writeln!(pdf, "{} 0 obj", index + 1)?;
        pdf.extend(object);
        write!(pdf, "\nendobj\n")?;
    }
    let start = pdf.len();
    write!(pdf, "xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1)?;
    for offset in &offsets {
        writeln!(pdf, "{offset:010} 00000 n ")?;
    }
    write!(
        pdf,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
        offsets.len() + 1
    )?;
    Ok(pdf)
}

#[tokio::test]
async fn sandboxed_pdf_extraction_validates_content_and_bounds_output() -> TestResult<()> {
    assert_eq!(
        extract_pdf(&document("LOCAL_PDF_CONTENT", 1)?)
            .await
            .map_err(|e| e.message)?
            .trim(),
        "LOCAL_PDF_CONTENT"
    );
    for bytes in [
        b"not pdf".to_vec(),
        b"%PDF-1.4 broken".to_vec(),
        document("", 1)?,
        [b"%PDF-".as_slice(), &vec![0; 10 * 1024 * 1024]].concat(),
    ] {
        assert_eq!(
            extract_pdf(&bytes)
                .await
                .err()
                .ok_or("unreadable input")?
                .status,
            415
        );
    }
    assert_eq!(
        extract_pdf(&document(&"X".repeat(500), 600)?)
            .await
            .err()
            .ok_or("oversized output")?
            .status,
        413
    );
    Ok(())
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!("bs-pdf-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn pdf_cancellation_terminates_launcher_and_descendants_with_open_pipes() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let launcher = scratch.0.join("launcher");
    let marker = scratch.0.join("pids");
    fs::write(
        &launcher,
        format!(
            "#!/bin/sh\n/bin/sleep 60 &\nprintf '%s %s\\n' \"$$\" \"$!\" > '{}'\nwait\n",
            marker.display()
        ),
    )?;
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700))?;
    let launch = || {
        let path = launcher.clone();
        tokio::spawn(async move { extract_pdf_with(&path, b"%PDF-fixture").await })
    };
    let pending = launch();
    pending.abort();
    assert!(pending.await.is_err_and(|e| e.is_cancelled()));
    assert!(!marker.exists());
    let pending = launch();
    let pids = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(text) = fs::read_to_string(&marker) {
                let pids = text
                    .split_whitespace()
                    .filter_map(|pid| pid.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                if pids.len() == 2 {
                    break pids;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    pending.abort();
    assert!(pending.await.is_err_and(|e| e.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(3), async {
        while pids.iter().any(|pid| {
            fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|line| {
                line.rsplit_once(") ")
                    .is_some_and(|(_, fields)| !fields.starts_with('Z'))
            })
        }) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn pdf_invalidation_discards_late_bytes_and_clears_cache() -> TestResult<()> {
    let scratch = Scratch::new()?;
    let config: crate::config::Config =
        serde_json::from_value(json!({"database":scratch.0.join("state.sqlite")}))?;
    let database = crate::db::open(&config.database, false, 0)?;
    database.connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS files(id TEXT PRIMARY KEY,deleted INTEGER,digest TEXT);
        CREATE TABLE IF NOT EXISTS pdf_text(file TEXT PRIMARY KEY,text TEXT);",
    )?;
    let id = uuid::Uuid::new_v4().to_string();
    database.connection.execute(
        "INSERT INTO files(id,deleted,digest) VALUES(?,0,'old')",
        [&id],
    )?;
    let computer = Arc::new(crate::computer::Manager::new(&config)?);
    let state = AppState {
        database: Arc::new(Mutex::new(database)),
        config: Arc::new(config),
        origin: "http://127.0.0.1".into(),
        auth: None,
        events: crate::events::Hub::new(),
        computer,
        turns: crate::turns::Runtime::default(),
        whatsapp: Default::default(),
        trial_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let (started, reading) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let old = json!({"id":id,"digest":"old"});
    let read = pdf_with(&state, &old, async {
        let _ = started.send(());
        released.await.map_err(|_| Failure::storage())?;
        Ok("Old PDF".into())
    });
    let invalidate = async {
        reading.await?;
        let db = state.database.lock().map_err(|_| "DB lock poisoned")?;
        forget(&db.connection, &id).map_err(|e| e.message)?;
        db.connection
            .execute("UPDATE files SET digest='new' WHERE id=?", [&id])?;
        release.send(()).map_err(|_| "Reader did not wait")?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    };
    let (old, invalidated) = tokio::join!(read, invalidate);
    invalidated?;
    assert_eq!(old.err().ok_or("expired extraction")?.status, 409);
    let cache = || -> TestResult<Option<String>> {
        use rusqlite::OptionalExtension;
        let db = state.database.lock().map_err(|_| "DB lock poisoned")?;
        Ok(db
            .connection
            .query_row("SELECT text FROM pdf_text WHERE file=?", [&id], |r| {
                r.get(0)
            })
            .optional()?)
    };
    assert_eq!(cache()?, None);
    let new = json!({"id":id,"digest":"new"});
    pdf_with(&state, &new, async { Ok("New PDF".into()) })
        .await
        .map_err(|e| e.message)?;
    assert_eq!(cache()?.as_deref(), Some("New PDF"));
    let db = state.database.lock().map_err(|_| "DB lock poisoned")?;
    forget(&db.connection, &id).map_err(|e| e.message)?;
    drop(db);
    assert_eq!(cache()?, None);
    Ok(())
}
