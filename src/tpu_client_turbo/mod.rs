pub use connection_cache::ConnectionCache;
use tpu_client_local::TpuClient as BackendTpuClient;
use tpu_client_local_2::{Result, TpuClientConfig};
pub mod connection_cache;
pub mod tpu_client_local;
pub mod tpu_client_local_2;
use {
    solana_connection_cache::connection_cache::{
        ConnectionCache as BackendConnectionCache, ConnectionManager, ConnectionPool,
        NewConnectionConfig,
    },
    solana_quic_client::{QuicConfig, QuicConnectionManager, QuicPool},
    solana_rpc_client::nonblocking::rpc_client::RpcClient,
    solana_sdk::{
        transaction::Transaction,
        transport::Result as TransportResult,
    },
    std::sync::Arc,
};

/// Client which sends transactions directly to the current leader's TPU port over UDP.
/// The client uses RPC to determine the current leader and fetch node contact info
pub struct TpuClient<
    P, // ConnectionPool
    M, // ConnectionManager
    C, // NewConnectionConfig
> {
    tpu_client: BackendTpuClient<P, M, C>,
}

impl<P, M, C> TpuClient<P, M, C>
where
    P: ConnectionPool<NewConnectionConfig = C>,
    M: ConnectionManager<ConnectionPool = P, NewConnectionConfig = C>,
    C: NewConnectionConfig,
{
    /// Serialize and send transaction to the current and upcoming leader TPUs according to fanout
    /// size
    #[allow(dead_code)]
    pub async fn send_transaction(&self, transaction: &Transaction) -> bool {
        self.tpu_client.send_transaction(transaction).await
    }

    /// Send a wire transaction to the current and upcoming leader TPUs according to fanout size
    #[allow(dead_code)]
    pub async fn send_wire_transaction(&self, wire_transaction: Vec<u8>) -> bool {
        self.tpu_client
            .send_wire_transaction(wire_transaction)
            .await
    }

    /// Serialize and send transaction to the current and upcoming leader TPUs according to fanout
    /// size
    /// Returns the last error if all sends fail
    #[allow(dead_code)]
    pub async fn try_send_transaction(&self, transaction: &Transaction) -> TransportResult<()> {
        self.tpu_client.try_send_transaction(transaction).await
    }


    /// Send a wire transaction to the current and upcoming leader TPUs according to fanout size
    /// Returns the last error if all sends fail
    pub async fn send_transaction_to_upcoming_leaders(
        &self,
        wire_transaction: Vec<u8>,
    ) -> TransportResult<()> {
        self.tpu_client
            .send_transaction_to_upcoming_leaders(wire_transaction)
            .await
    }

    /// Send a wire transaction to the current and upcoming leader TPUs according to fanout size
    /// Returns the last error if all sends fail
    #[allow(dead_code)]
    pub async fn try_send_wire_transaction(
        &self,
        wire_transaction: Vec<u8>,
    ) -> TransportResult<()> {
        self.tpu_client
            .try_send_wire_transaction(wire_transaction)
            .await
    }

    /// Send a batch of wire transactions to the current and upcoming leader TPUs according to
    /// fanout size
    /// Returns the last error if all sends fail
    #[allow(dead_code)]
    pub async fn try_send_wire_transaction_batch(
        &self,
        wire_transactions: Vec<Vec<u8>>,
    ) -> TransportResult<()> {
        self.tpu_client
            .try_send_wire_transaction_batch(wire_transactions)
            .await
    }
}

impl TpuClient<QuicPool, QuicConnectionManager, QuicConfig> {
    /// Create a new client that disconnects when dropped
    pub async fn new(
        name: &'static str,
        rpc_client: Arc<RpcClient>,
        websocket_url: &str,
        config: TpuClientConfig,
    ) -> Result<Self> {
        let connection_cache = match ConnectionCache::new(name) {
            ConnectionCache::Quic(cache) => cache,
        };
        Self::new_with_connection_cache(rpc_client, websocket_url, config, connection_cache).await
    }
}

impl<P, M, C> TpuClient<P, M, C>
where
    P: ConnectionPool<NewConnectionConfig = C>,
    M: ConnectionManager<ConnectionPool = P, NewConnectionConfig = C>,
    C: NewConnectionConfig,
{
    /// Create a new client that disconnects when dropped
    pub async fn new_with_connection_cache(
        rpc_client: Arc<RpcClient>,
        websocket_url: &str,
        config: TpuClientConfig,
        connection_cache: Arc<BackendConnectionCache<P, M, C>>,
    ) -> Result<Self> {
        Ok(Self {
            tpu_client: BackendTpuClient::new_with_connection_cache(
                rpc_client,
                websocket_url,
                config,
                connection_cache,
            )
            .await
            .unwrap(),
        })
    }

    #[allow(dead_code)]
    pub fn rpc_client(&self) -> &RpcClient {
        self.tpu_client.rpc_client()
    }

    #[allow(dead_code)]
    pub async fn shutdown(&mut self) {
        self.tpu_client.shutdown().await
    }
}
