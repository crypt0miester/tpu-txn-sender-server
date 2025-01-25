use {
    super::tpu_client_local_2::{RecentLeaderSlots, TpuClientConfig, MAX_FANOUT_SLOTS},
    bincode::serialize,
    futures_util::{future::join_all, stream::StreamExt},
    log::*,
    solana_connection_cache::{
        connection_cache::{
            ConnectionCache, ConnectionManager, ConnectionPool, NewConnectionConfig, Protocol,
            DEFAULT_CONNECTION_POOL_SIZE,
        },
        nonblocking::client_connection::ClientConnection,
    },
    solana_pubsub_client::nonblocking::pubsub_client::{PubsubClient, PubsubClientError},
    solana_rpc_client::nonblocking::rpc_client::RpcClient,
    solana_rpc_client_api::{
        client_error::{Error as ClientError, ErrorKind, Result as ClientResult},
        request::RpcError,
        response::{RpcContactInfo, SlotUpdate},
    },
    solana_sdk::{
        clock::{Slot, DEFAULT_MS_PER_SLOT, NUM_CONSECUTIVE_LEADER_SLOTS},
        commitment_config::CommitmentConfig,
        epoch_info::EpochInfo,
        pubkey::Pubkey,
        quic::QUIC_PORT_OFFSET,
        signature::SignerError,
        transaction::Transaction,
        transport::{Result as TransportResult, TransportError},
    },
    solana_tpu_client::tpu_client::Result,
    std::{
        collections::{HashMap, HashSet},
        net::SocketAddr,
        str::FromStr,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, RwLock,
        },
    },
    thiserror::Error,
    tokio::{
        task::JoinHandle,
        time::{sleep, timeout, Duration, Instant},
    },
};

#[derive(Error, Debug)]
pub enum TpuSenderError {
    #[error("Pubsub error: {0:?}")]
    PubsubError(#[from] PubsubClientError),
    #[error("RPC error: {0:?}")]
    RpcError(#[from] ClientError),
    #[error("IO error: {0:?}")]
    IoError(#[from] std::io::Error),
    #[error("Signer error: {0:?}")]
    SignerError(#[from] SignerError),
    #[error("Custom error: {0}")]
    Custom(String),
}

struct LeaderTpuCacheUpdateInfo {
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

struct LeaderTpuCache {
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
    fn get_unique_leader_sockets(
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
    fn get_leader_sockets(
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

/// Client which sends transactions directly to the current leader's TPU port over UDP.
/// The client uses RPC to determine the current leader and fetch node contact info
pub struct TpuClient<
    P, // ConnectionPool
    M, // ConnectionManager
    C, // NewConnectionConfig
> {
    fanout_slots: u64,
    leader_tpu_service: LeaderTpuService,
    exit: Arc<AtomicBool>,
    rpc_client: Arc<RpcClient>,
    connection_cache: Arc<ConnectionCache<P, M, C>>,
}

async fn send_wire_transaction_to_addr<P, M, C>(
    connection_cache: &ConnectionCache<P, M, C>,
    addr: &SocketAddr,
    wire_transaction: Vec<u8>,
) -> TransportResult<()>
where
    P: ConnectionPool<NewConnectionConfig = C>,
    M: ConnectionManager<ConnectionPool = P, NewConnectionConfig = C>,
    C: NewConnectionConfig,
{
    let conn = connection_cache.get_nonblocking_connection(addr);
    conn.send_data(&wire_transaction).await
}

async fn send_wire_transaction_batch_to_addr<P, M, C>(
    connection_cache: &ConnectionCache<P, M, C>,
    addr: &SocketAddr,
    wire_transactions: &[Vec<u8>],
) -> TransportResult<()>
where
    P: ConnectionPool<NewConnectionConfig = C>,
    M: ConnectionManager<ConnectionPool = P, NewConnectionConfig = C>,
    C: NewConnectionConfig,
{
    let conn = connection_cache.get_nonblocking_connection(addr);
    conn.send_data_batch(wire_transactions).await
}

impl<P, M, C> TpuClient<P, M, C>
where
    P: ConnectionPool<NewConnectionConfig = C>,
    M: ConnectionManager<ConnectionPool = P, NewConnectionConfig = C>,
    C: NewConnectionConfig,
{
    /// Serialize and send transaction to the current and upcoming leader TPUs according to fanout
    /// size
    pub async fn send_transaction(&self, transaction: &Transaction) -> bool {
        let wire_transaction = serialize(transaction).expect("serialization should succeed");
        self.send_wire_transaction(wire_transaction).await
    }

    /// Send a wire transaction to the current and upcoming leader TPUs according to fanout size
    pub async fn send_wire_transaction(&self, wire_transaction: Vec<u8>) -> bool {
        self.try_send_wire_transaction(wire_transaction)
            .await
            .is_ok()
    }

    /// Serialize and send transaction to the current and upcoming leader TPUs according to fanout
    /// size
    /// Returns the last error if all sends fail
    pub async fn try_send_transaction(&self, transaction: &Transaction) -> TransportResult<()> {
        let wire_transaction = serialize(transaction).expect("serialization should succeed");
        self.try_send_wire_transaction(wire_transaction).await
    }

    pub async fn send_transaction_to_upcoming_leaders(
        &self,
        wire_transaction: Vec<u8>,
    ) -> TransportResult<()> {
        let leaders = self
            .leader_tpu_service
            .unique_leader_tpu_sockets(self.fanout_slots);

        for tpu_address in &leaders {
            let cache = &self.connection_cache;
            let conn = cache.get_nonblocking_connection(tpu_address);
            conn.send_data(&wire_transaction).await?;
        }

        Ok(())
    }

    /// Send a wire transaction to the current and upcoming leader TPUs according to fanout size
    /// Returns the last error if all sends fail
    pub async fn try_send_wire_transaction(
        &self,
        wire_transaction: Vec<u8>,
    ) -> TransportResult<()> {
        let leaders = self
            .leader_tpu_service
            .unique_leader_tpu_sockets(self.fanout_slots);
        let futures = leaders
            .iter()
            .map(|addr| {
                send_wire_transaction_to_addr(
                    &self.connection_cache,
                    addr,
                    wire_transaction.clone(),
                )
            })
            .collect::<Vec<_>>();
        let results: Vec<TransportResult<()>> = join_all(futures).await;

        let mut last_error: Option<TransportError> = None;
        let mut some_success = false;
        for result in results {
            if let Err(e) = result {
                if last_error.is_none() {
                    last_error = Some(e);
                }
            } else {
                some_success = true;
            }
        }
        if !some_success {
            Err(if let Some(err) = last_error {
                err
            } else {
                std::io::Error::new(std::io::ErrorKind::Other, "No sends attempted").into()
            })
        } else {
            Ok(())
        }
    }

    /// Send a batch of wire transactions to the current and upcoming leader TPUs according to
    /// fanout size
    /// Returns the last error if all sends fail
    pub async fn try_send_wire_transaction_batch(
        &self,
        wire_transactions: Vec<Vec<u8>>,
    ) -> TransportResult<()> {
        let leaders = self
            .leader_tpu_service
            .unique_leader_tpu_sockets(self.fanout_slots);
        let futures = leaders
            .iter()
            .map(|addr| {
                send_wire_transaction_batch_to_addr(
                    &self.connection_cache,
                    addr,
                    &wire_transactions,
                )
            })
            .collect::<Vec<_>>();
        let results: Vec<TransportResult<()>> = join_all(futures).await;

        let mut last_error: Option<TransportError> = None;
        let mut some_success = false;
        for result in results {
            if let Err(e) = result {
                if last_error.is_none() {
                    last_error = Some(e);
                }
            } else {
                some_success = true;
            }
        }
        if !some_success {
            Err(if let Some(err) = last_error {
                err
            } else {
                std::io::Error::new(std::io::ErrorKind::Other, "No sends attempted").into()
            })
        } else {
            Ok(())
        }
    }

    /// Create a new client that disconnects when dropped
    pub async fn new(
        name: &'static str,
        rpc_client: Arc<RpcClient>,
        websocket_url: &str,
        config: TpuClientConfig,
        connection_manager: M,
    ) -> Result<Self> {
        let connection_cache = Arc::new(
            ConnectionCache::new(name, connection_manager, DEFAULT_CONNECTION_POOL_SIZE).unwrap(),
        ); // TODO: Handle error properly, as the ConnectionCache ctor is now fallible.
        Self::new_with_connection_cache(rpc_client, websocket_url, config, connection_cache).await
    }

    /// Create a new client that disconnects when dropped
    pub async fn new_with_connection_cache(
        rpc_client: Arc<RpcClient>,
        websocket_url: &str,
        config: TpuClientConfig,
        connection_cache: Arc<ConnectionCache<P, M, C>>,
    ) -> Result<Self> {
        let exit = Arc::new(AtomicBool::new(false));
        let leader_tpu_service =
            LeaderTpuService::new(rpc_client.clone(), websocket_url, M::PROTOCOL, exit.clone())
                .await?;

        Ok(Self {
            fanout_slots: config.fanout_slots.clamp(1, MAX_FANOUT_SLOTS),
            leader_tpu_service,
            exit,
            rpc_client,
            connection_cache,
        })
    }

    pub fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client
    }

    pub async fn shutdown(&mut self) {
        self.exit.store(true, Ordering::Relaxed);
        self.leader_tpu_service.join().await;
    }

    pub fn get_connection_cache(&self) -> &Arc<ConnectionCache<P, M, C>>
    where
        P: ConnectionPool<NewConnectionConfig = C>,
        M: ConnectionManager<ConnectionPool = P, NewConnectionConfig = C>,
        C: NewConnectionConfig,
    {
        &self.connection_cache
    }

    pub fn get_leader_tpu_service(&self) -> &LeaderTpuService {
        &self.leader_tpu_service
    }

    pub fn get_fanout_slots(&self) -> u64 {
        self.fanout_slots
    }
}

impl<P, M, C> Drop for TpuClient<P, M, C> {
    fn drop(&mut self) {
        self.exit.store(true, Ordering::Relaxed);
    }
}

/// Service that tracks upcoming leaders and maintains an up-to-date mapping
/// of leader id to TPU socket address.
pub struct LeaderTpuService {
    recent_slots: RecentLeaderSlots,
    leader_tpu_cache: Arc<RwLock<LeaderTpuCache>>,
    t_leader_tpu_service: Option<JoinHandle<Result<()>>>,
}

impl LeaderTpuService {
    pub async fn new(
        rpc_client: Arc<RpcClient>,
        websocket_url: &str,
        protocol: Protocol,
        exit: Arc<AtomicBool>,
    ) -> Result<Self> {
        let start_slot = rpc_client
            .get_slot_with_commitment(CommitmentConfig::processed())
            .await?;

        let recent_slots = RecentLeaderSlots::new(start_slot);
        let slots_in_epoch = rpc_client.get_epoch_info().await?.slots_in_epoch;

        // When a cluster is starting, we observe an invalid slot range failure that goes away after a
        // retry. It seems as if the leader schedule is not available, but it should be. The logic
        // below retries the RPC call in case of an invalid slot range error.
        let tpu_leader_service_creation_timeout = Duration::from_secs(20);
        let retry_interval = Duration::from_secs(1);
        let leaders = timeout(tpu_leader_service_creation_timeout, async {
            loop {
                // TODO: The root cause appears to lie within the `rpc_client.get_slot_leaders()`.
                // It might be worth debugging further and trying to understand why the RPC
                // call fails. There may be a bug in the `get_slot_leaders()` logic or in the
                // RPC implementation
                match rpc_client
                    .get_slot_leaders(start_slot, LeaderTpuCache::fanout(slots_in_epoch))
                    .await
                {
                    Ok(leaders) => return Ok(leaders),
                    Err(client_error) => {
                        if is_invalid_slot_range_error(&client_error) {
                            sleep(retry_interval).await;
                            continue;
                        } else {
                            return Err(client_error);
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            TpuSenderError::Custom(format!(
                "Failed to get slot leaders connecting to: {}, timeout: {:?}. Invalid slot range",
                websocket_url, tpu_leader_service_creation_timeout
            ))
        })
        .unwrap()
        .unwrap();

        let cluster_nodes = rpc_client.get_cluster_nodes().await?;
        let leader_tpu_cache = Arc::new(RwLock::new(LeaderTpuCache::new(
            start_slot,
            slots_in_epoch,
            leaders,
            cluster_nodes,
            protocol,
        )));

        let pubsub_client = if !websocket_url.is_empty() {
            Some(PubsubClient::new(websocket_url).await?)
        } else {
            None
        };

        let t_leader_tpu_service = Some({
            let recent_slots = recent_slots.clone();
            let leader_tpu_cache = leader_tpu_cache.clone();
            tokio::spawn(Self::run(
                rpc_client,
                recent_slots,
                leader_tpu_cache,
                pubsub_client,
                exit,
            ))
        });

        Ok(LeaderTpuService {
            recent_slots,
            leader_tpu_cache,
            t_leader_tpu_service,
        })
    }

    pub async fn join(&mut self) {
        if let Some(t_handle) = self.t_leader_tpu_service.take() {
            t_handle.await.unwrap().unwrap();
        }
    }

    #[allow(dead_code)]
    pub fn estimated_current_slot(&self) -> Slot {
        self.recent_slots.estimated_current_slot()
    }

    pub fn unique_leader_tpu_sockets(&self, fanout_slots: u64) -> Vec<SocketAddr> {
        let current_slot = self.recent_slots.estimated_current_slot();
        self.leader_tpu_cache
            .read()
            .unwrap()
            .get_unique_leader_sockets(current_slot, fanout_slots)
    }

    #[allow(dead_code)]
    pub fn leader_tpu_sockets(&self, fanout_slots: u64) -> Vec<SocketAddr> {
        let current_slot = self.recent_slots.estimated_current_slot();
        self.leader_tpu_cache
            .read()
            .unwrap()
            .get_leader_sockets(current_slot, fanout_slots)
    }

    async fn run(
        rpc_client: Arc<RpcClient>,
        recent_slots: RecentLeaderSlots,
        leader_tpu_cache: Arc<RwLock<LeaderTpuCache>>,
        pubsub_client: Option<PubsubClient>,
        exit: Arc<AtomicBool>,
    ) -> Result<()> {
        tokio::try_join!(
            Self::run_slot_watcher(recent_slots.clone(), pubsub_client, exit.clone()),
            Self::run_cache_refresher(rpc_client, recent_slots, leader_tpu_cache, exit),
        )?;

        Ok(())
    }

    async fn run_cache_refresher(
        rpc_client: Arc<RpcClient>,
        recent_slots: RecentLeaderSlots,
        leader_tpu_cache: Arc<RwLock<LeaderTpuCache>>,
        exit: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut last_cluster_refresh = Instant::now();
        let mut sleep_ms = DEFAULT_MS_PER_SLOT;

        while !exit.load(Ordering::Relaxed) {
            // Sleep a slot before checking if leader cache needs to be refreshed again
            sleep(Duration::from_millis(sleep_ms)).await;
            sleep_ms = DEFAULT_MS_PER_SLOT;

            let cache_update_info = maybe_fetch_cache_info(
                &leader_tpu_cache,
                last_cluster_refresh,
                &rpc_client,
                &recent_slots,
            )
            .await;

            if cache_update_info.has_some() {
                let mut leader_tpu_cache = leader_tpu_cache.write().unwrap();
                let (has_error, cluster_refreshed) = leader_tpu_cache
                    .update_all(recent_slots.estimated_current_slot(), cache_update_info);
                if has_error {
                    sleep_ms = 100;
                }
                if cluster_refreshed {
                    last_cluster_refresh = Instant::now();
                }
            }
        }

        Ok(())
    }

    async fn run_slot_watcher(
        recent_slots: RecentLeaderSlots,
        pubsub_client: Option<PubsubClient>,
        exit: Arc<AtomicBool>,
    ) -> Result<()> {
        let Some(pubsub_client) = pubsub_client else {
            return Ok(());
        };

        let (mut notifications, unsubscribe) = pubsub_client.slot_updates_subscribe().await?;
        // Time out slot update notification polling at 10ms.
        //
        // Rationale is two-fold:
        // 1. Notifications are an unbounded stream -- polling them will block indefinitely if not
        //    interrupted, and the exit condition will never be checked. 10ms ensures negligible
        //    CPU overhead while keeping notification checking timely.
        // 2. The timeout must be strictly less than the slot time (DEFAULT_MS_PER_SLOT: 400) to
        //    avoid timeout never being reached. For example, if notifications are received every
        //    400ms and the timeout is >= 400ms, notifications may theoretically always be available
        //    before the timeout is reached, resulting in the exit condition never being checked.
        const SLOT_UPDATE_TIMEOUT: Duration = Duration::from_millis(10);

        while !exit.load(Ordering::Relaxed) {
            while let Ok(Some(update)) = timeout(SLOT_UPDATE_TIMEOUT, notifications.next()).await {
                let current_slot = match update {
                    // This update indicates that a full slot was received by the connected
                    // node so we can stop sending transactions to the leader for that slot
                    SlotUpdate::Completed { slot, .. } => slot.saturating_add(1),
                    // This update indicates that we have just received the first shred from
                    // the leader for this slot and they are probably still accepting transactions.
                    SlotUpdate::FirstShredReceived { slot, .. } => slot,
                    _ => continue,
                };
                recent_slots.record_slot(current_slot);
            }
        }

        // `notifications` requires a valid reference to `pubsub_client`, so `notifications` must be
        // dropped before moving `pubsub_client` via `shutdown()`.
        drop(notifications);
        unsubscribe().await;
        pubsub_client.shutdown().await?;

        Ok(())
    }
}

async fn maybe_fetch_cache_info(
    leader_tpu_cache: &Arc<RwLock<LeaderTpuCache>>,
    last_cluster_refresh: Instant,
    rpc_client: &RpcClient,
    recent_slots: &RecentLeaderSlots,
) -> LeaderTpuCacheUpdateInfo {
    // Refresh cluster TPU ports every 5min in case validators restart with new port configuration
    // or new validators come online
    let maybe_cluster_nodes = if last_cluster_refresh.elapsed() > Duration::from_secs(5 * 60) {
        Some(rpc_client.get_cluster_nodes().await)
    } else {
        None
    };

    let estimated_current_slot = recent_slots.estimated_current_slot();
    let (last_slot, last_epoch_info_slot, slots_in_epoch) = {
        let leader_tpu_cache = leader_tpu_cache.read().unwrap();
        leader_tpu_cache.slot_info()
    };
    let maybe_epoch_info =
        if estimated_current_slot >= last_epoch_info_slot.saturating_sub(slots_in_epoch) {
            Some(rpc_client.get_epoch_info().await)
        } else {
            None
        };

    let maybe_slot_leaders = if estimated_current_slot >= last_slot.saturating_sub(MAX_FANOUT_SLOTS)
    {
        Some(
            rpc_client
                .get_slot_leaders(
                    estimated_current_slot,
                    LeaderTpuCache::fanout(slots_in_epoch),
                )
                .await,
        )
    } else {
        None
    };
    LeaderTpuCacheUpdateInfo {
        maybe_cluster_nodes,
        maybe_epoch_info,
        maybe_slot_leaders,
    }
}

fn is_invalid_slot_range_error(client_error: &ClientError) -> bool {
    if let ErrorKind::RpcError(RpcError::RpcResponseError { code, message, .. }) =
        &client_error.kind
    {
        return *code == -32602
            && message.contains("Invalid slot range: leader schedule for epoch");
    }
    false
}
