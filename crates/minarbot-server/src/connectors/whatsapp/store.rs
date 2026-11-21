use super::*;
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use whatsapp_rust::{
    async_trait,
    bytes::Bytes,
    wacore::{
        appstate::{hash::HashState, processor::AppStateMutationMAC},
        store::{error::StoreError, traits::*, Device},
    },
};

type Stored<T> = std::result::Result<T, StoreError>;

pub(super) struct Store {
    pub state: AppState,
    pub generation: String,
}

pub(super) fn encode<T: Serialize + ?Sized>(
    state: &AppState,
    name: &str,
    value: &T,
) -> Result<String> {
    let plain = serde_json::to_string(value).map_err(|_| Failure::storage())?;
    crate::mcp::client::encrypt_with(state, &plain, name, "whatsapp-key")
}

pub(super) fn decode<T: DeserializeOwned>(state: &AppState, name: &str, value: &str) -> Result<T> {
    let plain = crate::mcp::client::decrypt_with(state, value, name, "whatsapp-key")
        .map_err(|_| Failure::storage())?;
    serde_json::from_str(&plain).map_err(|_| Failure::storage())
}

fn unavailable<T>(_: T) -> StoreError {
    StoreError::Io(std::io::Error::other(
        "WhatsApp session storage unavailable",
    ))
}

fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn compound(parts: &[&str]) -> String {
    parts.join("\0")
}

impl Store {
    fn access<T>(&self, operation: impl FnOnce(&Connection) -> Stored<T>) -> Stored<T> {
        let db = lock(&self.state).map_err(unavailable)?;
        if !epoch_active(&db.connection, &self.generation).map_err(unavailable)? {
            return Err(unavailable(()));
        }
        operation(&db.connection)
    }

    fn batch<T>(&self, operation: impl FnOnce(&Connection) -> Stored<T>) -> Stored<T> {
        self.access(|db| {
            let tx = db.unchecked_transaction().map_err(unavailable)?;
            let result = operation(&tx)?;
            tx.commit().map_err(unavailable)?;
            Ok(result)
        })
    }

    fn decipher<T: DeserializeOwned>(&self, bucket: &str, key: &str, value: &str) -> Stored<T> {
        decode(&self.state, &format!("{bucket}:{key}"), value).map_err(unavailable)
    }

    fn read<T: DeserializeOwned>(
        &self,
        db: &Connection,
        bucket: &str,
        raw_key: &str,
    ) -> Stored<Option<T>> {
        let key = hash(raw_key);
        let value: Option<String> = db
            .query_row(
                "SELECT value FROM whatsapp_session WHERE bucket=? AND key=?",
                params![bucket, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(unavailable)?;
        value
            .map(|value| self.decipher(bucket, &key, &value))
            .transpose()
    }

    fn write<T: Serialize + ?Sized>(
        &self,
        db: &Connection,
        bucket: &str,
        raw_key: &str,
        value: &T,
        expires: i64,
    ) -> Stored<()> {
        let key = hash(raw_key);
        let value = encode(&self.state, &format!("{bucket}:{key}"), value).map_err(unavailable)?;
        db.execute(
            "INSERT OR REPLACE INTO whatsapp_session VALUES(?,?,?,?)",
            params![bucket, key, value, expires],
        )
        .map_err(unavailable)?;
        Ok(())
    }

    fn drop_key(db: &Connection, bucket: &str, key: &str) -> Stored<()> {
        db.execute(
            "DELETE FROM whatsapp_session WHERE bucket=? AND key=?",
            params![bucket, hash(key)],
        )
        .map_err(unavailable)?;
        Ok(())
    }

    fn scan<T: DeserializeOwned>(&self, db: &Connection, bucket: &str) -> Stored<Vec<T>> {
        let mut statement = db
            .prepare("SELECT key,value FROM whatsapp_session WHERE bucket=?")
            .map_err(unavailable)?;
        let rows = statement
            .query_map([bucket], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(unavailable)?;
        rows.map(|row| {
            let (key, value) = row.map_err(unavailable)?;
            self.decipher(bucket, &key, &value)
        })
        .collect()
    }

    fn get<T: DeserializeOwned>(&self, bucket: &str, key: &str) -> Stored<Option<T>> {
        self.access(|db| self.read(db, bucket, key))
    }
    fn put<T: Serialize + ?Sized>(&self, bucket: &str, key: &str, value: &T) -> Stored<()> {
        self.access(|db| self.write(db, bucket, key, value, 0))
    }
    fn timed(&self, bucket: &str, key: &str, value: &[u8]) -> Stored<()> {
        self.access(|db| self.write(db, bucket, key, value, self.state.config.now() / 1000))
    }
    fn remove(&self, bucket: &str, key: &str) -> Stored<()> {
        self.access(|db| Self::drop_key(db, bucket, key))
    }
    fn clear(&self, bucket: &str) -> Stored<()> {
        self.access(|db| {
            db.execute("DELETE FROM whatsapp_session WHERE bucket=?", [bucket])
                .map_err(unavailable)?;
            Ok(())
        })
    }
    fn expire(&self, bucket: &str, cutoff: i64) -> Stored<u32> {
        self.access(|db| {
            db.execute(
                "DELETE FROM whatsapp_session WHERE bucket=? AND expires>0 AND expires<?",
                params![bucket, cutoff],
            )
            .map_err(unavailable)
            .map(|count| count as u32)
        })
    }
}

#[async_trait]
impl DeviceStore for Store {
    async fn save(&self, device: &Device) -> Stored<()> {
        self.batch(|db| {
            self.write(db, "device", "", device, 0)?;
            if device.pn.is_some() || device.lid.is_some() {
                db.execute(
                    "UPDATE whatsapp_link SET state='linked' WHERE state='pairing'",
                    [],
                )
                .map_err(unavailable)?;
            }
            Ok(())
        })
    }
    async fn load(&self) -> Stored<Option<Device>> {
        self.get("device", "")
    }
    async fn exists(&self) -> Stored<bool> {
        Ok(self.load().await?.is_some())
    }
    async fn create(&self) -> Stored<i32> {
        self.batch(|db| {
            if self.read::<Device>(db, "device", "")?.is_none() {
                self.write(db, "device", "", &Device::new(), 0)?;
            }
            Ok(1)
        })
    }
}

type Prekey = (u32, Vec<u8>, bool);
#[async_trait]
impl SignalStore for Store {
    async fn put_identity(&self, address: &str, key: [u8; 32]) -> Stored<()> {
        self.put("identity", address, &key)
    }
    async fn put_identities_batch(&self, keys: &[(Arc<str>, [u8; 32])]) -> Stored<()> {
        self.batch(|db| {
            for (address, key) in keys {
                self.write(db, "identity", address, key, 0)?;
            }
            Ok(())
        })
    }
    async fn load_identity(&self, address: &str) -> Stored<Option<[u8; 32]>> {
        self.get("identity", address)
    }
    async fn delete_identity(&self, address: &str) -> Stored<()> {
        self.remove("identity", address)
    }
    async fn put_session(&self, address: &str, session: &[u8]) -> Stored<()> {
        self.put("signal", address, session)
    }
    async fn put_sessions_batch(&self, sessions: &[(Arc<str>, Bytes)]) -> Stored<()> {
        self.batch(|db| {
            for (address, session) in sessions {
                self.write(db, "signal", address, session.as_ref(), 0)?;
            }
            Ok(())
        })
    }
    async fn get_session(&self, address: &str) -> Stored<Option<Bytes>> {
        Ok(self.get::<Vec<u8>>("signal", address)?.map(Bytes::from))
    }
    async fn delete_session(&self, address: &str) -> Stored<()> {
        self.remove("signal", address)
    }
    async fn store_prekey(&self, id: u32, record: &[u8], uploaded: bool) -> Stored<()> {
        self.put("prekey", &id.to_string(), &(id, record, uploaded))
    }
    async fn store_prekeys_batch(&self, keys: &[(u32, Bytes)], uploaded: bool) -> Stored<()> {
        self.batch(|db| {
            for (id, record) in keys {
                self.write(
                    db,
                    "prekey",
                    &id.to_string(),
                    &(*id, record.as_ref(), uploaded),
                    0,
                )?;
            }
            Ok(())
        })
    }
    async fn load_prekey(&self, id: u32) -> Stored<Option<Bytes>> {
        Ok(self
            .get::<Prekey>("prekey", &id.to_string())?
            .map(|(_, key, _)| key.into()))
    }
    async fn mark_prekeys_uploaded(&self, ids: &[u32]) -> Stored<()> {
        self.batch(|db| {
            for id in ids {
                let key = id.to_string();
                if let Some((id, record, false)) = self.read::<Prekey>(db, "prekey", &key)? {
                    self.write(db, "prekey", &key, &(id, record, true), 0)?;
                }
            }
            Ok(())
        })
    }
    async fn remove_prekey(&self, id: u32) -> Stored<()> {
        self.remove("prekey", &id.to_string())
    }
    async fn get_max_prekey_id(&self) -> Stored<u32> {
        self.access(|db| {
            Ok(self
                .scan::<Prekey>(db, "prekey")?
                .into_iter()
                .map(|(id, _, _)| id)
                .max()
                .unwrap_or(0))
        })
    }
    async fn store_signed_prekey(&self, id: u32, record: &[u8]) -> Stored<()> {
        self.put("signed", &id.to_string(), &(id, record))
    }
    async fn load_signed_prekey(&self, id: u32) -> Stored<Option<Vec<u8>>> {
        Ok(self
            .get::<(u32, Vec<u8>)>("signed", &id.to_string())?
            .map(|(_, key)| key))
    }
    async fn load_all_signed_prekeys(&self) -> Stored<Vec<(u32, Vec<u8>)>> {
        self.access(|db| self.scan(db, "signed"))
    }
    async fn remove_signed_prekey(&self, id: u32) -> Stored<()> {
        self.remove("signed", &id.to_string())
    }
    async fn put_sender_key(&self, address: &str, record: &[u8]) -> Stored<()> {
        self.put("sender", address, record)
    }
    async fn put_sender_keys_batch(&self, keys: &[(Arc<str>, Bytes)]) -> Stored<()> {
        self.batch(|db| {
            for (address, key) in keys {
                self.write(db, "sender", address, key.as_ref(), 0)?;
            }
            Ok(())
        })
    }
    async fn get_sender_key(&self, address: &str) -> Stored<Option<Vec<u8>>> {
        self.get("sender", address)
    }
    async fn delete_sender_key(&self, address: &str) -> Stored<()> {
        self.remove("sender", address)
    }
}

fn mac_bucket(name: &str) -> String {
    format!("mac:{}", hash(name))
}

#[async_trait]
impl AppSyncStore for Store {
    async fn get_sync_key(&self, key_id: &[u8]) -> Stored<Option<AppStateSyncKey>> {
        self.get("sync-key", &hash(key_id))
    }
    async fn set_sync_key(&self, key_id: &[u8], key: AppStateSyncKey) -> Stored<()> {
        self.batch(|db| {
            self.write(db, "sync-key", &hash(key_id), &key, 0)?;
            let prior: Option<(i64, Vec<u8>)> = self.read(db, "sync-latest", "")?;
            if prior.is_none_or(|(time, _)| time <= key.timestamp) {
                self.write(db, "sync-latest", "", &(key.timestamp, key_id), 0)?;
            }
            Ok(())
        })
    }
    async fn get_version(&self, name: &str) -> Stored<HashState> {
        Ok(self.get("sync-version", name)?.unwrap_or_default())
    }
    async fn set_version(&self, name: &str, state: HashState) -> Stored<()> {
        self.put("sync-version", name, &state)
    }
    async fn put_mutation_macs(
        &self,
        name: &str,
        _version: u64,
        values: &[AppStateMutationMAC],
    ) -> Stored<()> {
        let bucket = mac_bucket(name);
        self.batch(|db| {
            for value in values {
                self.write(db, &bucket, &hash(&value.index_mac), &value.value_mac, 0)?;
            }
            Ok(())
        })
    }
    async fn get_mutation_mac(&self, name: &str, index_mac: &[u8]) -> Stored<Option<Vec<u8>>> {
        self.get(&mac_bucket(name), &hash(index_mac))
    }
    async fn delete_mutation_macs(&self, name: &str, index_macs: &[Vec<u8>]) -> Stored<()> {
        let bucket = mac_bucket(name);
        self.batch(|db| {
            for index in index_macs {
                Self::drop_key(db, &bucket, &hash(index))?;
            }
            Ok(())
        })
    }
    async fn clear_mutation_macs(&self, name: &str) -> Stored<()> {
        self.clear(&mac_bucket(name))
    }
    async fn get_latest_sync_key_id(&self) -> Stored<Option<Vec<u8>>> {
        Ok(self
            .get::<(i64, Vec<u8>)>("sync-latest", "")?
            .map(|(_, id)| id))
    }
}

type SenderDevices = (String, std::collections::BTreeMap<String, bool>);
#[async_trait]
impl ProtocolStore for Store {
    async fn get_sender_key_devices(&self, group: &str) -> Stored<Vec<(String, bool)>> {
        Ok(self
            .get::<SenderDevices>("sender-device", group)?
            .into_iter()
            .flat_map(|(_, entries)| entries)
            .collect())
    }
    async fn set_sender_key_status(&self, group: &str, entries: &[(&str, bool)]) -> Stored<()> {
        self.batch(|db| {
            let mut current = self
                .read::<SenderDevices>(db, "sender-device", group)?
                .map(|(_, entries)| entries)
                .unwrap_or_default();
            current.extend(
                entries
                    .iter()
                    .map(|(jid, present)| (jid.to_string(), *present)),
            );
            self.write(db, "sender-device", group, &(group, current), 0)
        })
    }
    async fn clear_sender_key_devices(&self, group: &str) -> Stored<()> {
        self.remove("sender-device", group)
    }
    async fn delete_sender_key_device_rows(&self, devices: &[&str]) -> Stored<()> {
        self.batch(|db| {
            for (group, mut entries) in self.scan::<SenderDevices>(db, "sender-device")? {
                let count = entries.len();
                entries.retain(|jid, _| !devices.contains(&jid.as_str()));
                if entries.len() != count {
                    self.write(db, "sender-device", &group, &(&group, entries), 0)?;
                }
            }
            Ok(())
        })
    }
    async fn clear_all_sender_key_devices(&self) -> Stored<()> {
        self.clear("sender-device")
    }
    async fn get_lid_mapping(&self, lid: &str) -> Stored<Option<LidPnMappingEntry>> {
        self.get("lid", lid)
    }
    async fn get_pn_mapping(&self, phone: &str) -> Stored<Option<LidPnMappingEntry>> {
        self.access(|db| {
            let Some(lid) = self.read::<String>(db, "phone", phone)? else {
                return Ok(None);
            };
            self.read(db, "lid", &lid)
        })
    }
    async fn put_lid_mapping(&self, entry: &LidPnMappingEntry) -> Stored<()> {
        self.batch(|db| {
            if let Some(previous) = self.read::<LidPnMappingEntry>(db, "lid", &entry.lid)? {
                if previous.phone_number != entry.phone_number {
                    Self::drop_key(db, "phone", &previous.phone_number)?;
                }
            }
            self.write(db, "lid", &entry.lid, entry, 0)?;
            self.write(db, "phone", &entry.phone_number, &entry.lid, 0)
        })
    }
    async fn get_all_lid_mappings(&self) -> Stored<Vec<LidPnMappingEntry>> {
        self.access(|db| self.scan(db, "lid"))
    }
    async fn save_base_key(&self, address: &str, id: &str, key: &[u8]) -> Stored<()> {
        self.put("base", &compound(&[address, id]), key)
    }
    async fn has_same_base_key(&self, address: &str, id: &str, key: &[u8]) -> Stored<bool> {
        Ok(self
            .get::<Vec<u8>>("base", &compound(&[address, id]))?
            .as_deref()
            == Some(key))
    }
    async fn delete_base_key(&self, address: &str, id: &str) -> Stored<()> {
        self.remove("base", &compound(&[address, id]))
    }
    async fn update_device_list(&self, record: DeviceListRecord) -> Stored<()> {
        self.put("devices", &record.user, &record)
    }
    async fn get_devices(&self, user: &str) -> Stored<Option<DeviceListRecord>> {
        self.get("devices", user)
    }
    async fn delete_devices(&self, user: &str) -> Stored<()> {
        self.remove("devices", user)
    }
    async fn get_tc_token(&self, jid: &str) -> Stored<Option<TcTokenEntry>> {
        Ok(self
            .get::<(String, TcTokenEntry)>("contact-token", jid)?
            .map(|(_, token)| token))
    }
    async fn put_tc_token(&self, jid: &str, token: &TcTokenEntry) -> Stored<()> {
        self.put("contact-token", jid, &(jid, token))
    }
    async fn delete_tc_token(&self, jid: &str) -> Stored<()> {
        self.remove("contact-token", jid)
    }
    async fn get_all_tc_token_jids(&self) -> Stored<Vec<String>> {
        self.access(|db| {
            Ok(self
                .scan::<(String, TcTokenEntry)>(db, "contact-token")?
                .into_iter()
                .map(|(jid, _)| jid)
                .collect())
        })
    }
    async fn delete_expired_tc_tokens(&self, token_cutoff: i64, sender_cutoff: i64) -> Stored<u32> {
        self.batch(|db| {
            let mut count = 0;
            for (jid, token) in self.scan::<(String, TcTokenEntry)>(db, "contact-token")? {
                if token.token_timestamp < token_cutoff
                    && token
                        .sender_timestamp
                        .is_none_or(|time| time < sender_cutoff)
                {
                    Self::drop_key(db, "contact-token", &jid)?;
                    count += 1;
                }
            }
            Ok(count)
        })
    }
    async fn store_sent_message(&self, chat: &str, id: &str, bytes: &[u8]) -> Stored<()> {
        self.timed("sent", &compound(&[chat, id]), bytes)
    }
    async fn take_sent_message(&self, chat: &str, id: &str) -> Stored<Option<Vec<u8>>> {
        let key = compound(&[chat, id]);
        self.batch(|db| {
            let value = self.read(db, "sent", &key)?;
            Self::drop_key(db, "sent", &key)?;
            Ok(value)
        })
    }
    async fn delete_expired_sent_messages(&self, cutoff: i64) -> Stored<u32> {
        self.expire("sent", cutoff)
    }
    async fn store_pending_inbound_batch(&self, rows: &[PendingInboundRow<'_>]) -> Stored<()> {
        let now = self.state.config.now() / 1000;
        self.batch(|db| {
            for row in rows {
                self.write(
                    db,
                    "pending",
                    &compound(&[row.chat, row.sender, row.id]),
                    row.message,
                    now,
                )?;
            }
            Ok(())
        })
    }
    async fn delete_pending_inbound_batch(&self, keys: &[PendingInboundKey<'_>]) -> Stored<()> {
        self.batch(|db| {
            for key in keys {
                Self::drop_key(db, "pending", &compound(&[key.chat, key.sender, key.id]))?;
            }
            Ok(())
        })
    }
    async fn store_pending_inbound(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
        bytes: &[u8],
    ) -> Stored<()> {
        self.timed("pending", &compound(&[chat, sender, id]), bytes)
    }
    async fn get_pending_inbound(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
    ) -> Stored<Option<Vec<u8>>> {
        self.get("pending", &compound(&[chat, sender, id]))
    }
    async fn delete_pending_inbound(&self, chat: &str, sender: &str, id: &str) -> Stored<()> {
        self.remove("pending", &compound(&[chat, sender, id]))
    }
    async fn delete_expired_pending_inbound(&self, cutoff: i64) -> Stored<u32> {
        self.expire("pending", cutoff)
    }
}

#[async_trait]
impl MsgSecretStore for Store {
    async fn put_msg_secrets(&self, entries: Vec<MsgSecretEntry>) -> Stored<usize> {
        self.batch(|db| {
            let count = entries.len();
            for entry in entries {
                let key = compound(&[&entry.chat, &entry.sender, &entry.msg_id]);
                let prior: Option<(MessageSecret, i64, i64)> = self.read(db, "secret", &key)?;
                let expires = prior.map_or(entry.expires_at, |(_, _, expiry)| {
                    merge_msg_secret_expiry(expiry, entry.expires_at)
                });
                let timestamp =
                    prior.map_or(entry.message_ts, |(_, time, _)| time.max(entry.message_ts));
                self.write(
                    db,
                    "secret",
                    &key,
                    &(entry.secret, timestamp, expires),
                    expires,
                )?;
            }
            Ok(count)
        })
    }
    async fn get_msg_secret(&self, chat: &str, sender: &str, id: &str) -> Stored<Option<Vec<u8>>> {
        Ok(self
            .get_msg_secret_with_ts(chat, sender, id)
            .await?
            .map(|(key, _)| key))
    }
    async fn get_msg_secret_with_ts(
        &self,
        chat: &str,
        sender: &str,
        id: &str,
    ) -> Stored<Option<(Vec<u8>, i64)>> {
        Ok(self
            .get::<(MessageSecret, i64, i64)>("secret", &compound(&[chat, sender, id]))?
            .map(|(key, time, _)| (key.to_vec(), time)))
    }
    async fn delete_expired_msg_secrets(&self, cutoff: i64) -> Stored<u32> {
        self.expire("secret", cutoff)
    }
}
