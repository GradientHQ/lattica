use std::collections::hash_map;

use cid::CidGeneric;
use fnv::{FnvHashMap, FnvHashSet};

use crate::message::{new_cancel_entry, new_want_block_entry, new_want_have_entry};
use crate::proto::message::mod_Message::Wantlist as ProtoWantlist;

#[derive(Debug)]
pub(crate) struct Wantlist<const S: usize> {
    cids: FnvHashSet<CidGeneric<S>>,
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
    force_update: bool,
    synced_revision: u64,
}

#[derive(Debug, Clone, Copy)]
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
            .and_modify(|state| *state = WantReqState::GotHave);
        self.force_update = true;
    }

    pub(crate) fn got_dont_have(&mut self, cid: &CidGeneric<S>) {
        self.req_state
            .entry(cid.to_owned())
            .and_modify(|state| *state = WantReqState::GotDontHave);
    }

    pub(crate) fn got_block(&mut self, cid: &CidGeneric<S>) {
        self.req_state
            .entry(cid.to_owned())
            .and_modify(|state| *state = WantReqState::GotBlock);
    }

    pub(crate) fn generate_proto_full(&mut self, wantlist: &Wantlist<S>) -> ProtoWantlist {
        // Remove canceled requests or received blocks
        self.req_state.retain(|cid, _| wantlist.cids.contains(cid));

        // Add new entries
        for cid in &wantlist.cids {
            self.req_state
                .entry(cid.to_owned())
                .or_insert(WantReqState::SentWantHave);
        }

        let mut entries = Vec::new();

        for (cid, req_state) in self.req_state.iter_mut() {
            match *req_state {
                WantReqState::SentWantHave => {
                    entries.push(new_want_have_entry(cid, wantlist.set_send_dont_have));
                }
                WantReqState::GotHave => {
                    entries.push(new_want_block_entry(cid, wantlist.set_send_dont_have));
                    *req_state = WantReqState::SentWantBlock;
                }
                WantReqState::GotDontHave => {
                    // Nothing to request
                }
                WantReqState::SentWantBlock => {
                    entries.push(new_want_block_entry(cid, wantlist.set_send_dont_have));
                }
                WantReqState::GotBlock => {
                    // Nothing to request
                }
            }
        }

        ProtoWantlist {
            entries,
            full: true,
        }
    }

    pub(crate) fn generate_proto_update(&mut self, wantlist: &Wantlist<S>) -> ProtoWantlist {
        if self.is_updated(wantlist) {
            return ProtoWantlist::default();
        }

        let mut entries = Vec::new();
        let mut removed = Vec::new();

        // Update existing entries
        for (cid, req_state) in self.req_state.iter_mut() {
            match (wantlist.cids.contains(cid), *req_state) {
                // If CID is not in the wantlist that means we received
                // its block from another peer. If we received a block
                // from this peer too then we don't need to send a cancel
                // message back.
                (false, WantReqState::GotBlock) => {
                    // Remove CID request state
                    removed.push(cid.to_owned());
                }

                // If CID is not in the wantlist that means we received
                // its block from another peer. We need to send a cancel
                // message to this peer.
                (false, _) => {
                    // Remove CID request state
                    removed.push(cid.to_owned());
                    // Add a cancel mesage
                    entries.push(new_cancel_entry(cid));
                }

                // WantHave was requested. We need a response to proceed further.
                (true, WantReqState::SentWantHave) => {}

                (true, WantReqState::GotHave) => {
                    entries.push(new_want_block_entry(cid, wantlist.set_send_dont_have));
                    *req_state = WantReqState::SentWantBlock;
                }

                // Server responsed with DontHave. We have nothing else to do.
                (true, WantReqState::GotDontHave) => {}

                // Block was requested
                (true, WantReqState::SentWantBlock) => {}

                // Block was received
                (true, WantReqState::GotBlock) => {}
            }
        }

        // Remove canceled requests or received blocks
        for cid in removed {
            self.req_state.remove(&cid);
        }

        // Add new entries
        for cid in &wantlist.cids {
            if let hash_map::Entry::Vacant(state_entry) = self.req_state.entry(cid.to_owned()) {
                entries.push(new_want_have_entry(cid, wantlist.set_send_dont_have));
                state_entry.insert(WantReqState::SentWantHave);
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
