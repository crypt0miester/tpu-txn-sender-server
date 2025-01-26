use {
    super::recent_leaders_slot::MAX_FANOUT_SLOTS,
    log::*,
    solana_client::connection_cache::Protocol,
    solana_rpc_client_api::{client_error::Result as ClientResult, response::RpcContactInfo},
    solana_sdk::{
        clock::{Slot, NUM_CONSECUTIVE_LEADER_SLOTS},
        epoch_info::EpochInfo,
        pubkey::Pubkey,
        quic::QUIC_PORT_OFFSET,
    },
    std::{
        collections::{HashMap, HashSet},
        net::SocketAddr,
        str::FromStr,
    },
};

pub struct LeaderTpuCacheUpdateInfo {
    pub(super) maybe_cluster_nodes: Option<ClientResult<Vec<RpcContactInfo>>>,
    pub(super) maybe_epoch_info: Option<ClientResult<EpochInfo>>,
    pub(super) maybe_slot_leaders: Option<ClientResult<Vec<Pubkey>>>,
}
impl LeaderTpuCacheUpdateInfo {
    pub fn has_some(&self) -> bool {
        self.maybe_cluster_nodes.is_some()
            || self.maybe_epoch_info.is_some()
            || self.maybe_slot_leaders.is_some()
    }
}

pub struct LeaderTpuCache {
    protocol: Protocol,
    first_slot: Slot,
    leaders: Vec<Pubkey>,
    leader_tpu_map: HashMap<Pubkey, SocketAddr>,
    slots_in_epoch: Slot,
    last_epoch_info_slot: Slot,
}

impl LeaderTpuCache {
    pub fn new(
        first_slot: Slot,
        slots_in_epoch: Slot,
        leaders: Vec<Pubkey>,
        cluster_nodes: Vec<RpcContactInfo>,
        protocol: Protocol,
    ) -> Self {
        let leader_tpu_map = Self::extract_cluster_tpu_sockets(protocol, cluster_nodes);
        Self {
            protocol,
            first_slot,
            leaders,
            leader_tpu_map,
            slots_in_epoch,
            last_epoch_info_slot: first_slot,
        }
    }

    // Last slot that has a cached leader pubkey
    pub fn last_slot(&self) -> Slot {
        self.first_slot + self.leaders.len().saturating_sub(1) as u64
    }

    pub fn slot_info(&self) -> (Slot, Slot, Slot) {
        (
            self.last_slot(),
            self.last_epoch_info_slot,
            self.slots_in_epoch,
        )
    }

    // Get the TPU sockets for the current leader and upcoming *unique* leaders according to fanout size.
    pub fn get_unique_leader_sockets(
        &self,
        estimated_current_slot: Slot,
        fanout_slots: u64,
    ) -> Vec<SocketAddr> {
        let all_leader_sockets = self.get_leader_sockets(estimated_current_slot, fanout_slots);

        let mut unique_sockets = Vec::new();
        let mut seen = HashSet::new();

        for socket in all_leader_sockets {
            if seen.insert(socket) {
                unique_sockets.push(socket);
            }
        }

        unique_sockets
    }

    // Get the TPU sockets for the current leader and upcoming leaders according to fanout size.
    pub fn get_leader_sockets(
        &self,
        estimated_current_slot: Slot,
        fanout_slots: u64,
    ) -> Vec<SocketAddr> {
        let mut leader_sockets = Vec::new();
        // `first_slot` might have been advanced since caller last read the `estimated_current_slot`
        // value. Take the greater of the two values to ensure we are reading from the latest
        // leader schedule.
        let current_slot = std::cmp::max(estimated_current_slot, self.first_slot);
        for leader_slot in (current_slot..current_slot + fanout_slots)
            .step_by(NUM_CONSECUTIVE_LEADER_SLOTS as usize)
        {
            if let Some(leader) = self.get_slot_leader(leader_slot) {
                if let Some(tpu_socket) = self.leader_tpu_map.get(leader) {
                    leader_sockets.push(*tpu_socket);
                } else {
                    // The leader is probably delinquent
                    trace!("TPU not available for leader {}", leader);
                }
            } else {
                // Overran the local leader schedule cache
                warn!(
                    "Leader not known for slot {}; cache holds slots [{},{}]",
                    leader_slot,
                    self.first_slot,
                    self.last_slot()
                );
            }
        }
        leader_sockets
    }

    pub fn get_slot_leader(&self, slot: Slot) -> Option<&Pubkey> {
        if slot >= self.first_slot {
            let index = slot - self.first_slot;
            self.leaders.get(index as usize)
        } else {
            None
        }
    }

    fn extract_cluster_tpu_sockets(
        protocol: Protocol,
        cluster_contact_info: Vec<RpcContactInfo>,
    ) -> HashMap<Pubkey, SocketAddr> {
        cluster_contact_info
            .into_iter()
            .filter_map(|contact_info| {
                let pubkey = Pubkey::from_str(&contact_info.pubkey).ok()?;
                let socket = match protocol {
                    Protocol::QUIC => contact_info.tpu_quic.or_else(|| {
                        let mut socket = contact_info.tpu?;
                        let port = socket.port().checked_add(QUIC_PORT_OFFSET)?;
                        socket.set_port(port);
                        Some(socket)
                    }),
                    Protocol::UDP => contact_info.tpu,
                }?;
                Some((pubkey, socket))
            })
            .collect()
    }

    pub fn fanout(slots_in_epoch: Slot) -> Slot {
        (2 * MAX_FANOUT_SLOTS).min(slots_in_epoch)
    }

    pub fn update_all(
        &mut self,
        estimated_current_slot: Slot,
        cache_update_info: LeaderTpuCacheUpdateInfo,
    ) -> (bool, bool) {
        let mut has_error = false;
        let mut cluster_refreshed = false;
        if let Some(cluster_nodes) = cache_update_info.maybe_cluster_nodes {
            match cluster_nodes {
                Ok(cluster_nodes) => {
                    self.leader_tpu_map =
                        Self::extract_cluster_tpu_sockets(self.protocol, cluster_nodes);
                    cluster_refreshed = true;
                }
                Err(err) => {
                    warn!("Failed to fetch cluster tpu sockets: {}", err);
                    has_error = true;
                }
            }
        }

        if let Some(Ok(epoch_info)) = cache_update_info.maybe_epoch_info {
            self.slots_in_epoch = epoch_info.slots_in_epoch;
            self.last_epoch_info_slot = estimated_current_slot;
        }

        if let Some(slot_leaders) = cache_update_info.maybe_slot_leaders {
            match slot_leaders {
                Ok(slot_leaders) => {
                    self.first_slot = estimated_current_slot;
                    self.leaders = slot_leaders;
                }
                Err(err) => {
                    warn!(
                        "Failed to fetch slot leaders (current estimated slot: {}): {}",
                        estimated_current_slot, err
                    );
                    has_error = true;
                }
            }
        }
        (has_error, cluster_refreshed)
    }
}
