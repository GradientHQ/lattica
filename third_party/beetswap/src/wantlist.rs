use std::collections::hash_map;

use cid::CidGeneric;
use fnv::{FnvHashMap, FnvHashSet};
use web_time::Instant;

use crate::message::{new_cancel_entry, new_want_block_entry, new_want_have_entry};
use crate::proto::message::mod_Message::Wantlist as ProtoWantlist;

#[derive(Debug)]
pub(crate) struct Wantlist<const S: usize> {
    pub(crate) cids: FnvHashSet<CidGeneric<S>>,
    revision: u64,
    set_send_dont_have: bool,
}

impl<const S: usize> Wantlist<S> {
    pub(crate) fn new(set_send_dont_have: bool) -> Self {
        Wantlist {
            cids: FnvHashSet::default(),
            revision: 0,
            set_send_dont_have,
        }
    }

    pub(crate) fn insert(&mut self, cid: CidGeneric<S>) -> bool {
        if self.cids.insert(cid) {
            self.revision += 1;
            true
        } else {
            false
        }
    }

    pub(crate) fn remove(&mut self, cid: &CidGeneric<S>) -> bool {
        if self.cids.remove(cid) {
            self.revision += 1;
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
pub(crate) struct WantlistState<const S: usize> {
    req_state: FnvHashMap<CidGeneric<S>, WantReqState>,
    /// Independent time cache for request start times, not affected by retain()
    /// Keeps recent entries to support dup block timing even after CID removal
    sent_time_cache: FnvHashMap<CidGeneric<S>, Instant>,
    force_update: bool,
    synced_revision: u64,
}

#[derive(Debug, Clone, PartialEq)]
enum WantReqState {
    SentWantHave,
    GotHave,
    GotDontHave,
    SentWantBlock,
    GotBlock,
}

impl<const S: usize> WantlistState<S> {
    pub(crate) fn new() -> Self {
        WantlistState {
            req_state: FnvHashMap::default(),
            sent_time_cache: FnvHashMap::default(),
            force_update: false,
            synced_revision: 0,
        }
    }

    pub(crate) fn is_updated(&self, wantlist: &Wantlist<S>) -> bool {
        !self.force_update && self.synced_revision == wantlist.revision
    }

    pub(crate) fn got_have(&mut self, cid: &CidGeneric<S>) {
        self.req_state
            .entry(cid.to_owned())
            .and_modify(|state| {
                // Only update if in SentWantHave state, preserve SentWantBlock/GotBlock
                if *state == WantReqState::SentWantHave {
                    *state = WantReqState::GotHave;
                }
            });
        self.force_update = true;
    }

    pub(crate) fn got_dont_have(&mut self, cid: &CidGeneric<S>) {
        self.req_state
            .entry(cid.to_owned())
            .and_modify(|state| {
                // Only update if in SentWantHave state, preserve SentWantBlock/GotBlock
                if *state == WantReqState::SentWantHave {
                    *state = WantReqState::GotDontHave;
                }
            });
    }

    pub(crate) fn got_block(&mut self, cid: &CidGeneric<S>) {
        self.req_state
            .entry(cid.to_owned())
            .and_modify(|state| {
                *state = WantReqState::GotBlock;
            });
    }

    /// Check if Have response was received (for candidate identification)
    /// Only returns true for GotHave state, not SentWantBlock
    pub(crate) fn has_received_have(&self, cid: &CidGeneric<S>) -> bool {
        self.req_state.get(cid) == Some(&WantReqState::GotHave)
    }
    
    /// Check if WantBlock was already sent
    pub(crate) fn has_sent_want_block(&self, cid: &CidGeneric<S>) -> bool {
        self.req_state.get(cid) == Some(&WantReqState::SentWantBlock)
    }

    /// Get the time when request was initiated for this CID and clear cache entry
    pub(crate) fn get_request_sent_time(&mut self, cid: &CidGeneric<S>) -> Option<Instant> {
        // Remove from cache - it's consumed after block is received
        self.sent_time_cache.remove(cid)
    }

    pub(crate) fn generate_proto_full(&mut self, wantlist: &Wantlist<S>) -> ProtoWantlist {
        self.generate_proto_full_with_filter(wantlist, None)
    }

    /// Generate full wantlist with selective WantBlock sending
    pub(crate) fn generate_proto_full_with_filter(
        &mut self,
        wantlist: &Wantlist<S>,
        should_send_want_block: Option<&FnvHashSet<CidGeneric<S>>>,
    ) -> ProtoWantlist {
        self.req_state.retain(|cid, _| wantlist.cids.contains(cid));

        let now = Instant::now();
        for cid in &wantlist.cids {
            if let std::collections::hash_map::Entry::Vacant(e) = self.req_state.entry(cid.to_owned()) {
                e.insert(WantReqState::SentWantHave);
                // Record in cache when first sending WantHave
                self.sent_time_cache.entry(cid.to_owned()).or_insert(now);
            }
        }

        let mut entries = Vec::new();

        for (cid, req_state) in self.req_state.iter_mut() {
            match req_state {
                WantReqState::SentWantHave => {
                    entries.push(new_want_have_entry(cid, wantlist.set_send_dont_have));
                }
                WantReqState::GotHave => {
                    let should_send = should_send_want_block.map(|set| set.contains(cid)).unwrap_or(true);
                    if should_send {
                        entries.push(new_want_block_entry(cid, wantlist.set_send_dont_have));
                        *req_state = WantReqState::SentWantBlock;
                    } else {
                        entries.push(new_want_have_entry(cid, wantlist.set_send_dont_have));
                    }
                }
                WantReqState::GotDontHave | WantReqState::GotBlock => {}
                WantReqState::SentWantBlock => {
                    entries.push(new_want_block_entry(cid, wantlist.set_send_dont_have));
                }
            }
        }

        ProtoWantlist { entries, full: true }
    }

    pub(crate) fn generate_proto_update(&mut self, wantlist: &Wantlist<S>) -> ProtoWantlist {
        self.generate_proto_update_with_filter(wantlist, None)
    }

    /// Generate incremental wantlist with selective WantBlock sending
    pub(crate) fn generate_proto_update_with_filter(
        &mut self,
        wantlist: &Wantlist<S>,
        should_send_want_block: Option<&FnvHashSet<CidGeneric<S>>>,
    ) -> ProtoWantlist {
        if self.is_updated(wantlist) {
            return ProtoWantlist::default();
        }

        let mut entries = Vec::new();
        let mut removed = Vec::new();

        for (cid, req_state) in self.req_state.iter_mut() {
            let in_wantlist = wantlist.cids.contains(cid);
            match (in_wantlist, &*req_state) {
                (false, WantReqState::GotBlock) => {
                    removed.push(cid.to_owned());
                }
                (false, _) => {
                    removed.push(cid.to_owned());
                    entries.push(new_cancel_entry(cid));
                }
                (true, WantReqState::SentWantHave) => {}
                (true, WantReqState::GotHave) => {
                    let should_send = should_send_want_block.map(|set| set.contains(cid)).unwrap_or(true);
                    if should_send {
                        entries.push(new_want_block_entry(cid, wantlist.set_send_dont_have));
                        *req_state = WantReqState::SentWantBlock;
                    }
                }
                (true, WantReqState::GotDontHave) => {}
                (true, WantReqState::SentWantBlock) => {}
                (true, WantReqState::GotBlock) => {}
            }
        }

        for cid in removed {
            self.req_state.remove(&cid);
        }

        let now = Instant::now();
        for cid in &wantlist.cids {
            if let hash_map::Entry::Vacant(state_entry) = self.req_state.entry(cid.to_owned()) {
                entries.push(new_want_have_entry(cid, wantlist.set_send_dont_have));
                state_entry.insert(WantReqState::SentWantHave);
                // Record in cache when first sending WantHave
                self.sent_time_cache.entry(cid.to_owned()).or_insert(now);
            }
        }

        self.synced_revision = wantlist.revision;
        self.force_update = false;

        ProtoWantlist {
            entries,
            full: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::message::mod_Message::mod_Wantlist::{Entry, WantType};
    use crate::test_utils::cid_of_data;

    #[test]
    fn insert() {
        let mut wantlist = Wantlist::<64>::new(true);
        let mut state = WantlistState::<64>::new();

        assert!(state.is_updated(&wantlist));
        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist::default()
        );

        let cid = cid_of_data(b"1");
        wantlist.insert(cid);
        assert!(!state.is_updated(&wantlist));

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![Entry {
                    block: cid.to_bytes(),
                    priority: 1,
                    cancel: false,
                    wantType: WantType::Have,
                    sendDontHave: true,
                }],
                full: false,
            }
        );
        assert!(state.is_updated(&wantlist));
    }

    #[test]
    fn handle_have() {
        let mut wantlist = Wantlist::<64>::new(true);
        let mut state = WantlistState::<64>::new();

        let cid1 = cid_of_data(b"1");
        let cid2 = cid_of_data(b"2");

        wantlist.insert(cid1);
        wantlist.insert(cid2);

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }
                ],
                full: false,
            }
        );
        assert!(state.is_updated(&wantlist));

        state.got_have(&cid1);
        assert!(!state.is_updated(&wantlist));

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![Entry {
                    block: cid1.to_bytes(),
                    priority: 1,
                    cancel: false,
                    wantType: WantType::Block,
                    sendDontHave: true,
                }],
                full: false,
            }
        );
        assert!(state.is_updated(&wantlist));

        assert_eq!(
            state.generate_proto_full(&wantlist),
            ProtoWantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Block,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }
                ],
                full: true,
            }
        );
    }

    #[test]
    fn handle_have_then_generate_only_full() {
        let mut wantlist = Wantlist::<64>::new(true);
        let mut state = WantlistState::<64>::new();

        let cid1 = cid_of_data(b"1");
        let cid2 = cid_of_data(b"2");

        wantlist.insert(cid1);
        wantlist.insert(cid2);

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }
                ],
                full: false,
            }
        );
        assert!(state.is_updated(&wantlist));

        state.got_have(&cid1);

        assert_eq!(
            state.generate_proto_full(&wantlist),
            ProtoWantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Block,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }
                ],
                full: true,
            }
        );
    }

    #[test]
    fn handle_dont_have() {
        let mut wantlist = Wantlist::<64>::new(true);
        let mut state = WantlistState::<64>::new();

        let cid1 = cid_of_data(b"1");
        let cid2 = cid_of_data(b"2");

        wantlist.insert(cid1);
        wantlist.insert(cid2);

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }
                ],
                full: false,
            }
        );
        assert!(state.is_updated(&wantlist));

        state.got_dont_have(&cid1);
        assert!(state.is_updated(&wantlist));

        assert_eq!(
            state.generate_proto_full(&wantlist),
            ProtoWantlist {
                entries: vec![Entry {
                    block: cid2.to_bytes(),
                    priority: 1,
                    cancel: false,
                    wantType: WantType::Have,
                    sendDontHave: true,
                }],
                full: true,
            }
        );
    }

    #[test]
    fn handle_block() {
        let mut wantlist = Wantlist::<64>::new(true);
        let mut state = WantlistState::<64>::new();

        let cid1 = cid_of_data(b"1");
        let cid2 = cid_of_data(b"2");

        wantlist.insert(cid1);
        wantlist.insert(cid2);

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }
                ],
                full: false,
            }
        );
        assert!(state.is_updated(&wantlist));

        state.got_block(&cid1);
        assert!(state.is_updated(&wantlist));

        assert_eq!(
            state.generate_proto_full(&wantlist),
            ProtoWantlist {
                entries: vec![Entry {
                    block: cid2.to_bytes(),
                    priority: 1,
                    cancel: false,
                    wantType: WantType::Have,
                    sendDontHave: true,
                }],
                full: true,
            }
        );
    }

    #[test]
    fn cancel_cid() {
        let mut wantlist = Wantlist::<64>::new(true);
        let mut state = WantlistState::<64>::new();

        let cid1 = cid_of_data(b"1");
        let cid2 = cid_of_data(b"2");

        wantlist.insert(cid1);
        wantlist.insert(cid2);

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![
                    Entry {
                        block: cid1.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    },
                    Entry {
                        block: cid2.to_bytes(),
                        priority: 1,
                        cancel: false,
                        wantType: WantType::Have,
                        sendDontHave: true,
                    }
                ],
                full: false,
            }
        );
        assert!(state.is_updated(&wantlist));

        wantlist.remove(&cid1);
        assert!(!state.is_updated(&wantlist));

        assert_eq!(
            state.generate_proto_update(&wantlist),
            ProtoWantlist {
                entries: vec![Entry {
                    block: cid1.to_bytes(),
                    cancel: true,
                    ..Default::default()
                }],
                full: false,
            }
        );

        assert_eq!(
            state.generate_proto_full(&wantlist),
            ProtoWantlist {
                entries: vec![Entry {
                    block: cid2.to_bytes(),
                    priority: 1,
                    cancel: false,
                    wantType: WantType::Have,
                    sendDontHave: true,
                }],
                full: true,
            }
        );
    }
}
