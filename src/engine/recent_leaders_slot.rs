use {
    super::backend_tpu_client::TpuSenderError,
    solana_sdk::clock::Slot,
    std::{
        collections::VecDeque,
        sync::{Arc, RwLock},
    },
};

#[allow(dead_code)]
pub const DEFAULT_TPU_ENABLE_UDP: bool = false;
#[allow(dead_code)]
pub const DEFAULT_TPU_USE_QUIC: bool = true;

/// The default connection count is set to 1 -- it should
/// be sufficient for most use cases. Validators can use
/// --tpu-connection-pool-size to override this default value.
#[allow(dead_code)]
pub const DEFAULT_TPU_CONNECTION_POOL_SIZE: usize = 1;

pub type Result<T> = std::result::Result<T, TpuSenderError>;

/// Default number of slots used to build TPU socket fanout set
pub const DEFAULT_FANOUT_SLOTS: u64 = 64;

/// Maximum number of slots used to build TPU socket fanout set
pub const MAX_FANOUT_SLOTS: u64 = 100;

/// Config params for `TpuClient`
#[derive(Clone, Debug)]
pub struct TpuClientConfig {
    /// The range of upcoming slots to include when determining which
    /// leaders to send transactions to (min: 1, max: `MAX_FANOUT_SLOTS`)
    pub fanout_slots: u64,
}

impl Default for TpuClientConfig {
    fn default() -> Self {
        Self {
            fanout_slots: DEFAULT_FANOUT_SLOTS,
        }
    }
}

// 48 chosen because it's unlikely that 12 leaders in a row will miss their slots
const MAX_SLOT_SKIP_DISTANCE: u64 = 48;

#[derive(Clone, Debug)]
pub(crate) struct RecentLeaderSlots(Arc<RwLock<VecDeque<Slot>>>);
impl RecentLeaderSlots {
    pub(crate) fn new(current_slot: Slot) -> Self {
        let mut recent_slots = VecDeque::new();
        recent_slots.push_back(current_slot);
        Self(Arc::new(RwLock::new(recent_slots)))
    }

    pub(crate) fn record_slot(&self, current_slot: Slot) {
        let mut recent_slots = self.0.write().unwrap();
        recent_slots.push_back(current_slot);
        // 12 recent slots should be large enough to avoid a misbehaving
        // validator from affecting the median recent slot
        while recent_slots.len() > 12 {
            recent_slots.pop_front();
        }
    }

    // Estimate the current slot from recent slot notifications.
    pub(crate) fn estimated_current_slot(&self) -> Slot {
        let mut recent_slots: Vec<Slot> = self.0.read().unwrap().iter().cloned().collect();
        assert!(!recent_slots.is_empty());
        recent_slots.sort_unstable();

        // Validators can broadcast invalid blocks that are far in the future
        // so check if the current slot is in line with the recent progression.
        let max_index = recent_slots.len() - 1;
        let median_index = max_index / 2;
        let median_recent_slot = recent_slots[median_index];
        let expected_current_slot = median_recent_slot + (max_index - median_index) as u64;
        let max_reasonable_current_slot = expected_current_slot + MAX_SLOT_SKIP_DISTANCE;

        // Return the highest slot that doesn't exceed what we believe is a
        // reasonable slot.
        recent_slots
            .into_iter()
            .rev()
            .find(|slot| *slot <= max_reasonable_current_slot)
            .unwrap()
    }
}
