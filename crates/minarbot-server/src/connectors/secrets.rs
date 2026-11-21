use super::*;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(bytes)
}
fn file_key(state: &AppState, sidecar: &str, create: bool) -> Result<Vec<u8>> {
    let path = minarbot_launcher::files::sidecar(&state.config.database, sidecar);
    if create {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(&random::<32>()?)
                    .map_err(|_| Failure::storage())?;
                f.sync_all().map_err(|_| Failure::storage())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(Failure::storage()),
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0o400000 | 0o4000)
        .open(path)
        .map_err(|_| Failure::storage())?;
    let stat = file.metadata().map_err(|_| Failure::storage())?;
    if !stat.is_file()
        || stat.nlink() != 1
        || stat.uid()
            != std::fs::metadata("/proc/self")
                .map_err(|_| Failure::storage())?
                .uid()
        || stat.mode() & 0o077 != 0
        || stat.len() != 32
    {
        return Err(Failure::storage());
    }
    let mut bytes = vec![];
    file.take(33)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::storage())?;
    if bytes.len() != 32 {
        return Err(Failure::storage());
    }
    Ok(bytes)
}
pub(super) fn encrypt(
    state: &AppState,
    sidecar: &str,
    context: &[u8],
    text: &str,
) -> Result<String> {
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, &file_key(state, sidecar, true)?)
            .map_err(|_| Failure::storage())?,
    );
    let nonce = random::<12>()?;
    let mut bytes = text.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(context),
        &mut bytes,
    )
    .map_err(|_| Failure::storage())?;
    Ok(B64.encode([nonce.to_vec(), bytes].concat()))
}
pub(super) fn decrypt(
    state: &AppState,
    sidecar: &str,
    context: &[u8],
    text: &str,
) -> Result<String> {
    let mut bytes = B64.decode(text).map_err(|_| Failure::storage())?;
    if bytes.len() < 28 {
        return Err(Failure::storage());
    }
    let nonce: [u8; 12] = bytes[..12].try_into().map_err(|_| Failure::storage())?;
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, &file_key(state, sidecar, false)?)
            .map_err(|_| Failure::storage())?,
    );
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(context),
            &mut bytes[12..],
        )
        .map_err(|_| Failure::storage())?;
    String::from_utf8(plain.to_vec()).map_err(|_| Failure::storage())
}
