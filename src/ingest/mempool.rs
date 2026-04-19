use std::collections::BTreeMap;

use crate::model::{PendingTransaction, TxIdentity};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingState {
    Active,
    Replaced { by_hash: String },
    Included { block_number: u64 },
    Dropped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRecord {
    pub tx: PendingTransaction,
    pub state: PendingState,
    pub first_seen_seq: u64,
    pub last_seen_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObserveOutcome {
    NewActive {
        tx_hash: String,
    },
    Duplicate {
        tx_hash: String,
    },
    Replaced {
        old_hash: String,
        new_hash: String,
    },
    IgnoredStaleReplacement {
        active_hash: String,
        stale_hash: String,
    },
}

#[derive(Default)]
pub struct MempoolTracker {
    next_seq: u64,
    records: BTreeMap<String, PendingRecord>,
    active_by_identity: BTreeMap<TxIdentity, String>,
}

impl MempoolTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, tx: PendingTransaction) -> ObserveOutcome {
        let seen_seq = self.bump_seq();

        if let Some(existing) = self.records.get_mut(&tx.tx_hash) {
            existing.last_seen_seq = seen_seq;
            return ObserveOutcome::Duplicate {
                tx_hash: tx.tx_hash,
            };
        }

        let identity = tx.identity();
        let tx_hash = tx.tx_hash.clone();

        if let Some(active_hash) = self.active_by_identity.get(&identity).cloned() {
            let active_rank = self
                .records
                .get(&active_hash)
                .map(|record| record.tx.replacement_rank())
                .unwrap_or((0, 0));

            if tx.replacement_rank() <= active_rank {
                return ObserveOutcome::IgnoredStaleReplacement {
                    active_hash,
                    stale_hash: tx_hash,
                };
            }

            if let Some(active_record) = self.records.get_mut(&active_hash) {
                active_record.state = PendingState::Replaced {
                    by_hash: tx_hash.clone(),
                };
                active_record.last_seen_seq = seen_seq;
            }

            self.active_by_identity.insert(identity, tx_hash.clone());
            self.records.insert(
                tx_hash.clone(),
                PendingRecord {
                    tx,
                    state: PendingState::Active,
                    first_seen_seq: seen_seq,
                    last_seen_seq: seen_seq,
                },
            );

            return ObserveOutcome::Replaced {
                old_hash: active_hash,
                new_hash: tx_hash,
            };
        }

        self.active_by_identity.insert(identity, tx_hash.clone());
        self.records.insert(
            tx_hash.clone(),
            PendingRecord {
                tx,
                state: PendingState::Active,
                first_seen_seq: seen_seq,
                last_seen_seq: seen_seq,
            },
        );

        ObserveOutcome::NewActive { tx_hash }
    }

    pub fn mark_included(&mut self, tx_hash: &str, block_number: u64) -> bool {
        self.transition_terminal(tx_hash, PendingState::Included { block_number })
    }

    pub fn mark_dropped(&mut self, tx_hash: &str) -> bool {
        self.transition_terminal(tx_hash, PendingState::Dropped)
    }

    pub fn record(&self, tx_hash: &str) -> Option<&PendingRecord> {
        self.records.get(tx_hash)
    }

    pub fn active_transaction(&self, identity: TxIdentity) -> Option<&PendingTransaction> {
        let hash = self.active_by_identity.get(&identity)?;
        self.records.get(hash).map(|record| &record.tx)
    }

    pub fn active_transactions(&self) -> Vec<&PendingTransaction> {
        self.active_by_identity
            .values()
            .filter_map(|hash| self.records.get(hash).map(|record| &record.tx))
            .collect()
    }

    pub fn active_len(&self) -> usize {
        self.active_by_identity.len()
    }

    fn transition_terminal(&mut self, tx_hash: &str, next_state: PendingState) -> bool {
        let seen_seq = self.bump_seq();
        let Some(record) = self.records.get_mut(tx_hash) else {
            return false;
        };

        if self
            .active_by_identity
            .get(&record.tx.identity())
            .is_some_and(|active_hash| active_hash == tx_hash)
        {
            self.active_by_identity.remove(&record.tx.identity());
        }

        record.state = next_state;
        record.last_seen_seq = seen_seq;
        true
    }

    fn bump_seq(&mut self) -> u64 {
        self.next_seq = self.next_seq.saturating_add(1);
        self.next_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Address;

    fn tx(
        tx_hash: &str,
        from: Address,
        nonce: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> PendingTransaction {
        PendingTransaction {
            tx_hash: tx_hash.to_string(),
            from,
            nonce,
            to: Some(Address::new([0x22; 20])),
            max_fee_per_gas,
            max_priority_fee_per_gas,
            input: vec![0xde, 0xad, 0xbe, 0xef],
        }
    }

    #[test]
    fn tracks_new_active_transaction() {
        let from = Address::new([0x11; 20]);
        let mut tracker = MempoolTracker::new();

        let outcome = tracker.observe(tx("0x1", from, 7, 100, 2));

        assert_eq!(
            outcome,
            ObserveOutcome::NewActive {
                tx_hash: "0x1".to_string()
            }
        );
        assert_eq!(tracker.active_len(), 1);
        assert_eq!(
            tracker.active_transaction(TxIdentity { from, nonce: 7 }),
            Some(&tx("0x1", from, 7, 100, 2))
        );
    }

    #[test]
    fn deduplicates_same_hash() {
        let from = Address::new([0x11; 20]);
        let mut tracker = MempoolTracker::new();
        tracker.observe(tx("0x1", from, 7, 100, 2));

        let outcome = tracker.observe(tx("0x1", from, 7, 100, 2));

        assert_eq!(
            outcome,
            ObserveOutcome::Duplicate {
                tx_hash: "0x1".to_string()
            }
        );
        assert_eq!(tracker.active_len(), 1);
        assert_eq!(tracker.record("0x1").unwrap().first_seen_seq, 1);
        assert_eq!(tracker.record("0x1").unwrap().last_seen_seq, 2);
    }

    #[test]
    fn higher_fee_transaction_replaces_active_identity() {
        let from = Address::new([0x11; 20]);
        let mut tracker = MempoolTracker::new();
        tracker.observe(tx("0x1", from, 7, 100, 2));

        let outcome = tracker.observe(tx("0x2", from, 7, 120, 3));

        assert_eq!(
            outcome,
            ObserveOutcome::Replaced {
                old_hash: "0x1".to_string(),
                new_hash: "0x2".to_string()
            }
        );
        assert_eq!(
            tracker.record("0x1").unwrap().state,
            PendingState::Replaced {
                by_hash: "0x2".to_string()
            }
        );
        assert_eq!(tracker.active_len(), 1);
        assert_eq!(
            tracker
                .active_transaction(TxIdentity { from, nonce: 7 })
                .unwrap()
                .tx_hash,
            "0x2"
        );
    }

    #[test]
    fn ignores_lower_fee_stale_replacement() {
        let from = Address::new([0x11; 20]);
        let mut tracker = MempoolTracker::new();
        tracker.observe(tx("0x2", from, 7, 120, 3));

        let outcome = tracker.observe(tx("0x1", from, 7, 100, 2));

        assert_eq!(
            outcome,
            ObserveOutcome::IgnoredStaleReplacement {
                active_hash: "0x2".to_string(),
                stale_hash: "0x1".to_string()
            }
        );
        assert_eq!(tracker.active_len(), 1);
        assert!(tracker.record("0x1").is_none());
    }

    #[test]
    fn inclusion_removes_active_identity() {
        let from = Address::new([0x11; 20]);
        let mut tracker = MempoolTracker::new();
        tracker.observe(tx("0x1", from, 7, 100, 2));

        assert!(tracker.mark_included("0x1", 123));
        assert_eq!(tracker.active_len(), 0);
        assert_eq!(
            tracker.record("0x1").unwrap().state,
            PendingState::Included { block_number: 123 }
        );
        assert!(tracker
            .active_transaction(TxIdentity { from, nonce: 7 })
            .is_none());
    }

    #[test]
    fn drop_marks_transaction_terminal() {
        let from = Address::new([0x11; 20]);
        let mut tracker = MempoolTracker::new();
        tracker.observe(tx("0x1", from, 7, 100, 2));

        assert!(tracker.mark_dropped("0x1"));
        assert_eq!(tracker.active_len(), 0);
        assert_eq!(tracker.record("0x1").unwrap().state, PendingState::Dropped);
    }
}
