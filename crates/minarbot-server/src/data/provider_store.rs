#![allow(dead_code)]

use super::Result;
use crate::{operator_file, web::Failure, AppState};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use std::{io::ErrorKind, path::Path};

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Failure::storage())?;
    Ok(bytes)
}

fn wrapping_key(database: &Path, create: bool) -> Result<LessSafeKey> {
    let parent = database
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| Failure::storage())?;
    let directory = operator_file::open_directory(&parent).map_err(|_| Failure::storage())?;
    let key_path = minarbot_launcher::files::sidecar(database, ".provider-key");
    let name = key_path.file_name().ok_or_else(Failure::storage)?;
    let file = match operator_file::file_at(&directory, name) {
        Err(error) if create && error.kind() == ErrorKind::NotFound => {
            operator_file::publish(&directory, name, &random::<32>()?)
                .map_err(|_| Failure::storage())?;
            operator_file::file_at(&directory, name)
        }
        result => result,
    }
    .map_err(|_| Failure::storage())?;
    if !operator_file::private(&file) {
        return Err(Failure::storage());
    }
    let bytes: [u8; 32] = operator_file::bounded_read(file, 32)
        .map_err(|_| Failure::storage())?
        .try_into()
        .map_err(|_| Failure::storage())?;
    Ok(LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| Failure::storage())?,
    ))
}

fn decrypt(database: &Path, id: &str, ciphertext: &str) -> Option<String> {
    let key = wrapping_key(database, false).ok()?;
    let mut bytes = B64.decode(ciphertext).ok()?;
    let nonce = bytes.get(..12)?.try_into().ok()?;
    let plain = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(id.as_bytes()),
            &mut bytes[12..],
        )
        .ok()?;
    String::from_utf8(plain.to_vec()).ok()
}

fn encrypt(database: &Path, id: &str, value: &str) -> Option<String> {
    let key = wrapping_key(database, true).ok()?;
    let nonce = random::<12>().ok()?;
    let mut bytes = value.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(id.as_bytes()),
        &mut bytes,
    )
    .ok()?;
    Some(B64.encode([nonce.as_slice(), &bytes].concat()))
}

pub(crate) fn encrypt_secret(state: &AppState, id: &str, value: &str) -> Result<String> {
    encrypt(&state.config.database, id, value).ok_or_else(Failure::storage)
}

pub(crate) fn decrypt_secret(state: &AppState, id: &str, value: &str) -> Option<String> {
    decrypt(&state.config.database, id, value)
}

pub(crate) fn reencrypt_legacy_secret(
    database: &Path,
    account_id: &str,
    instance_id: &str,
    ciphertext: &str,
) -> Option<String> {
    let value = decrypt(database, account_id, ciphertext)?;
    encrypt(database, instance_id, &value)
}

#[cfg(test)]
pub(crate) fn encrypt_legacy_secret(
    database: &Path,
    id: &str,
    value: &str,
) -> crate::db::Result<String> {
    encrypt(database, id, value).ok_or_else(|| "Cannot encrypt legacy fixture secret".into())
}
