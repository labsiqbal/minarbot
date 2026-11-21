use crate::web::Failure;
use crate::AppState;
use image::ImageDecoder;
use rusqlite::Connection;
use std::io::Cursor;
use tokio::sync::Semaphore;

static DECODERS: Semaphore = Semaphore::const_new(2);

#[derive(Default)]
pub struct Prepared {
    input: Option<Vec<u8>>,
    output: Option<Vec<u8>>,
}
impl Prepared {
    pub fn get(&mut self, input: Option<Vec<u8>>) -> Result<Option<Vec<u8>>, Failure> {
        let Some(bytes) = input else {
            return Ok(None);
        };
        if let Some(output) = &self.output {
            return Ok(Some(output.clone()));
        }
        self.input = Some(bytes);
        Err(Failure::storage())
    }
}
async fn decode(bytes: Vec<u8>) -> Result<Vec<u8>, Failure> {
    blocking(std::time::Duration::from_secs(3), move |deadline| {
        normalize(&bytes, deadline)
    })
    .await
}
async fn blocking(
    duration: std::time::Duration,
    work: impl FnOnce(std::time::Instant) -> Result<Vec<u8>, Failure> + Send + 'static,
) -> Result<Vec<u8>, Failure> {
    let slot = DECODERS.try_acquire().map_err(|_| {
        Failure::new(
            429,
            "invalid_avatar",
            "Image processing is busy. Try again.",
        )
    })?;
    let deadline = std::time::Instant::now() + duration;
    let task = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        work(deadline)
    });
    tokio::time::timeout(duration, task)
        .await
        .map_err(|_| failed())?
        .map_err(|_| failed())?
}
fn failed() -> Failure {
    Failure::new(
        400,
        "invalid_avatar",
        "Cannot read the image. Use a static PNG, JPEG, or WebP up to 16 megapixels.",
    )
}
fn normalize(bytes: &[u8], deadline: std::time::Instant) -> Result<Vec<u8>, Failure> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| failed())?;
    let format = image::guess_format(bytes).map_err(|_| failed())?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
    ) {
        return Err(failed());
    }
    let mut decoder = reader.into_decoder().map_err(|_| failed())?;
    let orientation = decoder.orientation().map_err(|_| failed())?;
    let profile = decoder.icc_profile().map_err(|_| failed())?;
    if u64::from(decoder.dimensions().0) * u64::from(decoder.dimensions().1) > 16 * 1024 * 1024 {
        return Err(failed());
    }
    if format == image::ImageFormat::WebP
        && image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))
            .map_err(|_| failed())?
            .has_animation()
    {
        return Err(failed());
    }
    let mut image = image::DynamicImage::from_decoder(decoder).map_err(|_| failed())?;
    if std::time::Instant::now() >= deadline {
        return Err(failed());
    }
    if let Some(profile) = profile.and_then(|p| moxcms::ColorProfile::new_from_slice(&p).ok()) {
        let mut rgba = image.to_rgba8();
        let transform = profile
            .create_transform_8bit(
                moxcms::Layout::Rgba,
                &moxcms::ColorProfile::new_srgb(),
                moxcms::Layout::Rgba,
                moxcms::TransformOptions::default(),
            )
            .map_err(|_| failed())?;
        let mut output = vec![0; rgba.len()];
        let stride = rgba.width() as usize * 4;
        for (src, dst) in rgba
            .chunks_exact(stride)
            .zip(output.chunks_exact_mut(stride))
        {
            if std::time::Instant::now() >= deadline {
                return Err(failed());
            }
            transform.transform(src, dst).map_err(|_| failed())?;
        }
        rgba.as_mut().copy_from_slice(&output);
        image = if image.has_alpha() {
            image::DynamicImage::ImageRgba8(rgba)
        } else {
            image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(rgba).to_rgb8())
        };
    }
    image.apply_orientation(orientation);
    let side = image.width().min(image.height());
    image = image.crop_imm(
        (image.width() - side).div_ceil(2),
        (image.height() - side).div_ceil(2),
        side,
        side,
    );
    let alpha = matches!(
        image.color(),
        image::ColorType::Rgba8
            | image::ColorType::Rgba16
            | image::ColorType::La8
            | image::ColorType::La16
    );
    let mut rgba = image.to_rgba32f();
    if alpha {
        for pixel in rgba.pixels_mut() {
            for c in 0..3 {
                pixel[c] *= pixel[3];
            }
        }
    }
    let mut resized =
        image::imageops::resize(&rgba, 256, 256, image::imageops::FilterType::Lanczos3);
    if alpha {
        for pixel in resized.pixels_mut() {
            if pixel[3] > 0.0 {
                for c in 0..3 {
                    pixel[c] = (pixel[c] / pixel[3]).clamp(0.0, 1.0);
                }
            }
        }
    }
    let resized = if alpha {
        image::DynamicImage::ImageRgba8(image::DynamicImage::ImageRgba32F(resized).to_rgba8())
    } else {
        image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba32F(resized).to_rgb8())
    };
    let mut output = Vec::new();
    resized
        .write_to(&mut Cursor::new(&mut output), image::ImageFormat::Png)
        .map_err(|_| failed())?;
    if std::time::Instant::now() >= deadline {
        return Err(failed());
    }
    Ok(output)
}
pub async fn run<T>(
    state: &AppState,
    mut work: impl FnMut(&Connection, &mut Prepared) -> Result<T, Failure>,
) -> Result<T, Failure> {
    let mut prepared = Prepared::default();
    let result = {
        let guard = state.database.lock().map_err(|_| Failure::storage())?;
        work(&guard.connection, &mut prepared)
    };
    let Some(input) = prepared.input.take() else {
        return result;
    };
    prepared.output = Some(decode(input).await?);
    let guard = state.database.lock().map_err(|_| Failure::storage())?;
    #[cfg(test)]
    tests::after_decode(&guard.connection)?;
    work(&guard.connection, &mut prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;
    use std::{
        cell::RefCell,
        sync::{Arc, Mutex},
    };
    use tokio::sync::Mutex as AsyncMutex;

    type DecodeHook = fn(&Connection) -> Result<(), Failure>;
    thread_local! {
        static AFTER_DECODE: RefCell<Option<DecodeHook>> = const { RefCell::new(None) };
    }
    static DECODER_TEST: AsyncMutex<()> = AsyncMutex::const_new(());

    pub(super) fn after_decode(db: &Connection) -> Result<(), Failure> {
        match AFTER_DECODE.take() {
            Some(hook) => hook(db),
            None => Ok(()),
        }
    }

    #[tokio::test]
    async fn avatar_decode_rechecks_cancel_and_revision_before_commit() -> crate::db::Result<()> {
        let _decoder = DECODER_TEST.lock().await;
        struct TestDirectory(std::path::PathBuf);
        impl Drop for TestDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = TestDirectory(
            std::env::temp_dir().join(format!("minarbot-avatar-{}", uuid::Uuid::new_v4())),
        );
        let config: crate::config::Config =
            serde_json::from_value(json!({"database":directory.0.join("avatar.sqlite")}))?;
        let database = crate::db::open(&config.database, false, 0)?;
        crate::data::initialize(&database)?;
        crate::files::init(&database.connection)?;
        crate::turns::initialize(&database.connection).map_err(|e| e.message)?;
        crate::mcp::initialize(&database.connection).map_err(|e| e.message)?;
        let db = &database.connection;
        let file = uuid::Uuid::new_v4().to_string();
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 8).write_to(&mut png, image::ImageFormat::Png)?;
        db.execute_batch(
            "INSERT INTO agents(id,name,model,instructions) VALUES('agent','Agent','','')",
        )?;
        db.execute(
            "INSERT INTO files(id,scope,name,digest,bytes,created) VALUES(?,'agents:agent','image.png','',?,'')",
            params![file, png.into_inner()],
        )?;
        db.execute(
            "INSERT INTO turns(id,agent,text,status,created,attachments) VALUES('turn','agent','','running','',?)",
            [json!([file]).to_string()],
        )?;
        let computer = Arc::new(crate::computer::Manager::new(&config).map_err(|e| e.to_string())?);
        let state = AppState {
            database: Arc::new(Mutex::new(database)),
            config: Arc::new(config),
            origin: "http://127.0.0.1".to_owned(),
            auth: None,
            events: crate::events::Hub::new(),
            computer,
            turns: crate::turns::Runtime::default(),
            whatsapp: Default::default(),
            trial_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let turn = json!({"id":"turn","agent":"agent"});
        let call = |id| json!({"id":id,"function":{"name":"set_avatar","arguments":json!({"image":file,"revision":0}).to_string()}});

        AFTER_DECODE.set(Some(|db| {
            db.execute("UPDATE turns SET status='cancelled' WHERE id='turn'", [])?;
            Ok(())
        }));
        let stopped = crate::turns::tools::run(&state, &turn, &call("cancel"))
            .await
            .err()
            .ok_or("Avatar committed after cancellation")?;
        assert_eq!(stopped.message, "Task has stopped.");
        {
            let guard = state
                .database
                .lock()
                .map_err(|_| "Fixture database lock poisoned")?;
            let db = &guard.connection;
            for table in ["agent_avatars", "avatar_commands", "flow_tool_calls"] {
                let count: i64 =
                    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
                assert_eq!(count, 0, "{table}");
            }
            db.execute("UPDATE turns SET status='running' WHERE id='turn'", [])?;
        }

        AFTER_DECODE.set(Some(|db| {
            db.execute("INSERT INTO agent_avatars VALUES('agent',1,NULL)", [])?;
            Ok(())
        }));
        let conflict = crate::turns::tools::run(&state, &turn, &call("conflict"))
            .await
            .err()
            .ok_or("Avatar committed over a newer revision")?;
        assert_eq!(conflict.status, 409);
        let guard = state
            .database
            .lock()
            .map_err(|_| "Fixture database lock poisoned")?;
        let db = &guard.connection;
        assert_eq!(
            super::super::agents::avatar_read(db, "agent").map_err(|e| e.message)?,
            json!({"revision":1,"present":false})
        );
        let count: i64 = db.query_row("SELECT count(*) FROM avatar_commands", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        let count: i64 = db.query_row("SELECT count(*) FROM flow_tool_calls", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        Ok(())
    }
    #[test]
    fn narrow_images_crop_before_scaling_without_unbounded_allocation(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = image::RgbImage::from_pixel(65535, 1, image::Rgb([70, 90, 120]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(source)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
        let output = normalize(
            &bytes,
            std::time::Instant::now() + std::time::Duration::from_secs(3),
        )
        .map_err(|e| e.message)?;
        let decoded = image::load_from_memory(&output)?;
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
        assert!(decoded
            .to_rgb8()
            .pixels()
            .all(|pixel| pixel.0 == [70, 90, 120]));
        Ok(())
    }
    #[tokio::test]
    async fn decoder_admission_timeout_and_failure_release_capacity(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let _decoder = DECODER_TEST.lock().await;
        let first = DECODERS.try_acquire()?;
        let second = DECODERS.try_acquire()?;
        let busy = decode(Vec::new())
            .await
            .err()
            .ok_or("Expected busy decoder")?;
        assert_eq!(busy.status, 429);
        assert_eq!(busy.message, "Image processing is busy. Try again.");
        drop(first);
        drop(second);
        let invalid = decode(Vec::new())
            .await
            .err()
            .ok_or("Expected malformed image")?;
        assert_eq!(invalid.status, 400);
        assert_eq!(invalid.message, failed().message);
        assert_eq!(DECODERS.available_permits(), 2);
        let (release, wait) = std::sync::mpsc::channel();
        let timeout = blocking(std::time::Duration::from_millis(20), move |_| {
            let _ = wait.recv();
            Ok(Vec::new())
        })
        .await;
        assert_eq!(
            timeout.err().ok_or("Expected timeout")?.message,
            failed().message
        );
        assert_eq!(DECODERS.available_permits(), 1);
        release.send(())?;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while DECODERS.available_permits() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        Ok(())
    }
}
