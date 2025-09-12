use std::borrow::Cow;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use libp2p::kad::store::{MemoryStore, RecordStore, Error};
use libp2p::kad::{ProviderRecord, Record, RecordKey};
use libp2p::PeerId;
use sled::Db;
use anyhow::{anyhow, Result};
use bincode::{Decode, Encode};
use tokio::task::JoinHandle;

pub struct MultiStore {
    pub memory_store: Arc<RwLock<MemoryStore>>,
    pub persistent_store: Arc<RwLock<Db>>,
    _cleanup_handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Encode, Decode)]
struct StoredRecord {
    key: Vec<u8>,
    value: Vec<u8>,
    publisher: Option<Vec<u8>>,
    expires: Option<u64>
}

impl From<Record> for StoredRecord {
    fn from(record: Record) -> Self {
        let expires = record.expires.map(|t| {
            let now = Instant::now();
            let duration = t.saturating_duration_since(now);
            let unix_ts = SystemTime::now()
                .checked_add(duration)
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            unix_ts
        });


        Self {
            key: record.key.as_ref().to_vec(),
            value: record.value.clone(),
            publisher: Some(record.publisher.unwrap().to_bytes()),
            expires,
        }
    }
}

impl From<StoredRecord> for Record {
    fn from(stored: StoredRecord) -> Self {
        let mut record = Record::new(
            RecordKey::new(&stored.key),
            stored.value,
        );

        record.publisher = stored.publisher.and_then(|bytes| {
            PeerId::from_bytes(&bytes).ok()
        });

        record.expires = stored.expires.map(|ts| {
            let now_system = SystemTime::now();
            let now_instant = Instant::now();
            let expire_system = UNIX_EPOCH + Duration::from_secs(ts);
            let delta = expire_system.duration_since(now_system).unwrap_or_default();
            now_instant + delta
        });

        record
    }
}

impl MultiStore {
    pub fn new(peer_id: PeerId, db_path: String) -> Result<Self> {
        let db = sled::open(db_path)?;
        
        let memory_store = Arc::new(RwLock::new(MemoryStore::new(peer_id)));

        let mut store = Self{
            memory_store,
            persistent_store: Arc::new(RwLock::new(db)),
            _cleanup_handle: None,
        };

        let memory_store_clone = store.memory_store.clone();
        let persistent_store_clone = store.persistent_store.clone();

        let cleanup_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            
            loop {
                interval.tick().await;

                if let Err(e) = Self::cleanup(
                    &memory_store_clone,
                    &persistent_store_clone
                ) {
                    tracing::error!("Failed to run store cleanup: {}", e);
                }
                tracing::debug!("Cleaning up finished");
            }
        });

        store._cleanup_handle = Some(cleanup_handle);

        Ok(store)
    }

    pub fn warm_up(&mut self) ->Result<()> {
        let db = self.persistent_store.read().map_err(|e| anyhow!(e.to_string()))?;
        let mut memory_store = self.memory_store.write().map_err(|e| anyhow!(e.to_string()))?;

        for result in db.iter() {
            let (_, value) = result?;

            let stored: StoredRecord = bincode::decode_from_slice(&value, bincode::config::standard())
                .map(|(data, _)| data)
                .map_err(|e| e)?;

            let record = stored.into();

            // only load not expired record
            if !self.is_expired(&record) {
                memory_store.put(record)?;
            }
        }
        
        Ok(())
    }

    fn is_expired(&self, record: &Record) -> bool {
        if let Some(expires) = record.expires {
            expires < Instant::now()
        } else {
            false
        }
    }

    fn cleanup(
        memory_store: &Arc<RwLock<MemoryStore>>,
        persistent_store: &Arc<RwLock<Db>>,
    ) ->Result<()> {
        let now_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        let now = Instant::now();

        // clean memory store
        if let Ok(mut memory_store) = memory_store.write() {
            let mut expired_keys = Vec::new();

            for record in memory_store.records() {
                if let Some(expires) = record.expires {
                    if expires <= now {
                        expired_keys.push(record.key.clone());
                    }
                }
            }

            for key in expired_keys {
                memory_store.remove(&key);
            }
        }

        // clean persistent store
        if let Ok(db) = persistent_store.write() {
            let mut expired_keys = Vec::new();

            for result in db.iter() {
                if let Ok((key, value)) = result {
                    if let Ok((stored, _)) = bincode::decode_from_slice::<StoredRecord, _>(&value, bincode::config::standard()) {
                        if let Some(expires_nanos) = stored.expires {
                            if expires_nanos <= now_ts {
                                expired_keys.push(key.clone());
                            }
                        }
                    }
                }
            }

            for key in expired_keys {
                let _ = db.remove(&key);
            }
        }

        Ok(())
    }
}

impl Drop for MultiStore {
    fn drop(&mut self) {
        if let Some(handle) = self._cleanup_handle.take() {
            handle.abort();
        }
    }
}

impl RecordStore for MultiStore {
    fn get(&self, k: &RecordKey) -> Option<Cow<'_, Record>> {
        if let Ok(mut memory_store) = self.memory_store.write() {
            if let Some(record ) = memory_store.get(k) {
                let record = record.into_owned();

                // check expired
                if self.is_expired(&record) {
                    memory_store.remove(k);
                    return None
                }

                return Some(Cow::Owned(record));
            }
        }

        let db = self.persistent_store.try_read().ok()?;
        if let Some(value) = db.get(k.as_ref()).ok().flatten() {
            if let Ok((stored, _)) = bincode::decode_from_slice::<StoredRecord, _>(&value, bincode::config::standard()) {
                let record: Record = stored.into();
                // check expired
                if self.is_expired(&record) {
                    if let Ok(mut db) = self.persistent_store.write() {
                        let _ = db.remove(k);
                    }

                    return None
                }

                if let Ok(mut memory_store) = self.memory_store.write() {
                    let _ = memory_store.put(record.clone());
                }
                return Some(Cow::Owned(record));
            }
        }
        None
    }

    fn put(&mut self, v: Record) -> Result<(), Error> {
        if let Ok(mut memory_store) = self.memory_store.write() {
            memory_store.put(v.clone())?;
        }


        let stored_record = StoredRecord::from(v.clone());
        let serialized = bincode::encode_to_vec(&stored_record, bincode::config::standard())
            .unwrap_or_else(|_e| Vec::new());

        let db = self.persistent_store.clone();

        tokio::spawn(async move {
            if let Ok(db) = db.write() {
                let _ = db.insert(v.key.as_ref(), serialized);
            }
        });

        Ok(())
    }

    fn remove(&mut self, k: &RecordKey){
        if let Ok(mut memory_store) = self.memory_store.write() {
            memory_store.remove(k);
        }

        if let Ok(db) = self.persistent_store.read() {
            let _ = db.remove(k.as_ref());
        }
    }

    fn add_provider(&mut self, record: ProviderRecord) -> Result<(), Error> {
        if let Ok(mut memory_store) = self.memory_store.write() {
            memory_store.add_provider(record);
        }

        Ok(())
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        if let Ok(memory_store) = self.memory_store.read() {
            let vec: Vec<ProviderRecord> = memory_store.provided().map(|c| c.into_owned()).collect();
            Box::new(vec.into_iter().map(Cow::Owned))
        } else {
            Box::new(std::iter::empty())
        }
    }

    fn providers(&self, key: &RecordKey) -> Vec<ProviderRecord> {
        if let Ok(memory_store) = self.memory_store.read() {
            return memory_store.providers(key);
        }
        vec![]
    }

    fn records(&self) -> Self::RecordsIter<'_> {
        let memory_iter = if let Ok(memory_store) = self.memory_store.read() {
            let records: Vec<Cow<'static, Record>> = memory_store.records()
                .filter_map(|cow| {
                    let record = cow.into_owned();
                    if self.is_expired(&record) {
                        None
                    } else {
                        Some(Cow::Owned(record))
                    }
                })
                .collect();
            Box::new(records.into_iter()) as Box<dyn Iterator<Item = Cow<'_, Record>>>
        } else {
            Box::new(std::iter::empty()) as Box<dyn Iterator<Item = Cow<'_, Record>>>
        };

        let db = self.persistent_store.try_read().ok();
        let persistent_iter = db.map(|db| db.iter());

        RecordsIterator {
            memory_iter,
            persistent_iter,
        }
    }

    fn remove_provider(&mut self, k: &RecordKey, p: &PeerId) {
        if let Ok(mut memory_store) = self.memory_store.write() {
            memory_store.remove_provider(k, p);
        }
    }

    type RecordsIter<'a> = RecordsIterator<'a>;
    type ProvidedIter<'a> = Box<dyn Iterator<Item = Cow<'a, ProviderRecord>> + 'a>;
}

pub struct RecordsIterator<'a> {
    memory_iter: Box<dyn Iterator<Item = Cow<'a, Record>> + 'a>,
    persistent_iter: Option<sled::Iter>,
}

impl<'a> Iterator for RecordsIterator<'a> {
    type Item = Cow<'a, Record>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(record) = self.memory_iter.next() {
            return Some(record);
        }

        if let Some(ref mut persistent_iter) = self.persistent_iter {
            while let Some(result) = persistent_iter.next() {
                if let Ok((_, value)) = result {
                    if let Ok((stored, _)) = bincode::decode_from_slice::<StoredRecord, _>(&value, bincode::config::standard()) {
                        let record: Record = stored.into();

                        // check expired
                        if let Some(expires) = record.expires {
                            if expires <= Instant::now() {
                                continue;
                            }
                        }
                        
                        return Some(Cow::Owned(record))
                    }
                }
            }
        }
        None
    }
}