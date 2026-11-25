//! PDF fixtures and a PDF parse gate (the former
//! `app/test-support/pdf-fixture.ts` and the Node `pdfFault` option).

use crate::{
    bridge::{Bridge, PdfFuture},
    Result,
};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// zlib stream of `data` in stored (uncompressed) deflate blocks: valid
/// `/FlateDecode` input without a compression dependency.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let length = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

/// A PDF with `pages` pages that each show `text` in Helvetica.
pub fn pdf_fixture(text: &str, pages: usize, compressed: bool) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            (0..pages)
                .map(|index| format!("{} 0 R", 4 + index * 2))
                .collect::<Vec<_>>()
                .join(" ")
        )
        .into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let escaped: String = text
        .chars()
        .flat_map(|c| match c {
            '\\' | '(' | ')' => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    for index in 0..pages {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10000 10000] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + index * 2
            )
            .into_bytes(),
        );
        let plain = format!("BT /F1 12 Tf 40 9900 Td ({escaped}) Tj ET").into_bytes();
        let stream = if compressed {
            zlib_stored(&plain)
        } else {
            plain
        };
        let mut object = format!(
            "<< /Length {}{} >>\nstream\n",
            stream.len(),
            if compressed {
                " /Filter /FlateDecode"
            } else {
                ""
            }
        )
        .into_bytes();
        object.extend_from_slice(&stream);
        object.extend_from_slice(b"\nendstream");
        objects.push(object);
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let start = out.len();
    let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
    for offset in &offsets {
        xref.push_str(&format!("{offset:010} 00000 n \n"));
    }
    xref.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
        offsets.len() + 1
    ));
    out.extend_from_slice(xref.as_bytes());
    out
}

/// One held parse: the next PDF the server parses waits for [`Hold::release`].
#[derive(Clone)]
pub struct Hold {
    started: watch::Sender<bool>,
    released: watch::Sender<bool>,
    stopped: watch::Sender<bool>,
}

impl Hold {
    fn new() -> Self {
        Self {
            started: watch::channel(false).0,
            released: watch::channel(false).0,
            stopped: watch::channel(false).0,
        }
    }

    async fn wait(sender: &watch::Sender<bool>, label: &str) -> Result<()> {
        let mut receiver = sender.subscribe();
        tokio::time::timeout(crate::WAIT, receiver.wait_for(|value| *value))
            .await
            .map_err(|_| {
                format!(
                    "PDF fixture parser did not {label} within {:?}",
                    crate::WAIT
                )
            })??;
        Ok(())
    }

    /// The server started parsing the held PDF.
    pub async fn started(&self) -> Result<()> {
        Self::wait(&self.started, "start").await
    }

    /// The server abandoned the held parse (it dropped the request).
    pub async fn stopped(&self) -> Result<()> {
        Self::wait(&self.stopped, "stop").await
    }

    pub fn release(&self) {
        self.released.send_replace(true);
    }
}

/// Marks a held parse stopped unless it finished normally.
struct Abandoned(Option<Hold>);

impl Drop for Abandoned {
    fn drop(&mut self) {
        if let Some(hold) = self.0.take() {
            hold.stopped.send_replace(true);
            hold.release();
        }
    }
}

#[derive(Default)]
struct GateState {
    fail_once: bool,
    armed: Option<Hold>,
}

/// PDF fault bridge for `ServerConfig::pdf_bridge`: fail the next parse once
/// or hold the next parse.
#[derive(Clone)]
pub struct PdfGate {
    bridge: Bridge,
    state: Arc<Mutex<GateState>>,
}

impl PdfGate {
    pub async fn start() -> Result<Self> {
        let state: Arc<Mutex<GateState>> = Arc::default();
        let shared = state.clone();
        let bridge = Bridge::pdf_async(move |_| {
            let (fail, hold) = match shared.lock() {
                Ok(mut state) => {
                    let fail = std::mem::take(&mut state.fail_once);
                    (fail, if fail { None } else { state.armed.take() })
                }
                Err(_) => (false, None),
            };
            let future: PdfFuture = Box::pin(async move {
                if fail {
                    return Some((503, "PDF sandbox is unavailable.".to_owned()));
                }
                if let Some(hold) = hold {
                    let mut guard = Abandoned(Some(hold.clone()));
                    hold.started.send_replace(true);
                    let mut released = hold.released.subscribe();
                    let _ = released.wait_for(|value| *value).await;
                    guard.0 = None;
                }
                None
            });
            future
        })
        .await?;
        Ok(Self { bridge, state })
    }

    pub fn bridge(&self) -> &Bridge {
        &self.bridge
    }

    /// The next parse fails with 503 "PDF sandbox is unavailable.".
    pub fn fail_once(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_once = true;
        }
    }

    /// Hold the next parse.
    pub fn hold(&self) -> Hold {
        let hold = Hold::new();
        if let Ok(mut state) = self.state.lock() {
            state.armed = Some(hold.clone());
        }
        hold
    }
}
