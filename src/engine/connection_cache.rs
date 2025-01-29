pub use solana_connection_cache::connection_cache::Protocol;
use {
    quinn::Endpoint,
    solana_connection_cache::{
        client_connection::ClientConnection,
        connection_cache::{
            BaseClientConnection, ConnectionCache as BackendConnectionCache, ConnectionPool,
            NewConnectionConfig,
        },
    },
    crate::quik::{QuicConfig, QuicConnectionManager, QuicPool},
    solana_sdk::{
        pubkey::Pubkey, quic::NotifyKeyUpdate, signature::Keypair,
        transport::Result as TransportResult,
    },
    solana_streamer::streamer::StakedNodes,
    std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{Arc, RwLock},
    },
};

const DEFAULT_CONNECTION_POOL_SIZE: usize = 4;
#[allow(dead_code)]
const DEFAULT_CONNECTION_CACHE_USE_QUIC: bool = true;

/// A thin wrapper over connection-cache/ConnectionCache to ease
/// construction of the ConnectionCache for code dealing both with udp and quic.
/// For the scenario only using udp or quic, use connection-cache/ConnectionCache directly.
pub enum ConnectionCache {
    Quic(Arc<BackendConnectionCache<QuicPool, QuicConnectionManager, QuicConfig>>),
}

type QuicBaseClientConnection = <QuicPool as ConnectionPool>::BaseClientConnection;

pub enum BlockingClientConnection {
    #[allow(dead_code)]
    Quic(Arc<<QuicBaseClientConnection as BaseClientConnection>::BlockingClientConnection>),
}

pub enum NonblockingClientConnection {
    #[allow(dead_code)]
    Quic(Arc<<QuicBaseClientConnection as BaseClientConnection>::NonblockingClientConnection>),
}

impl NotifyKeyUpdate for ConnectionCache {
    fn update_key(&self, key: &Keypair) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Self::Quic(backend) => backend.update_key(key),
        }
    }
}

impl ConnectionCache {
    pub fn new(name: &'static str) -> Self {
        let cert_info = (&Keypair::new(), IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)));
        ConnectionCache::new_with_client_options(
            name,
            DEFAULT_CONNECTION_POOL_SIZE,
            None, // client_endpoint
            Some(cert_info),
            None, // stake_info
        )
    }

    /// Create a quic connection_cache
    #[allow(dead_code)]
    pub fn new_quic(name: &'static str, connection_pool_size: usize) -> Self {
        Self::new_with_client_options(name, connection_pool_size, None, None, None)
    }

    /// Create a quic connection_cache with more client options
    pub fn new_with_client_options(
        name: &'static str,
        connection_pool_size: usize,
        client_endpoint: Option<Endpoint>,
        cert_info: Option<(&Keypair, IpAddr)>,
        stake_info: Option<(&Arc<RwLock<StakedNodes>>, &Pubkey)>,
    ) -> Self {
        // The minimum pool size is 1.
        let connection_pool_size = 1.max(connection_pool_size);
        let mut config = QuicConfig::new().unwrap();
        if let Some(client_endpoint) = client_endpoint {
            config.update_client_endpoint(client_endpoint);
        }
        if let Some(cert_info) = cert_info {
            config.update_client_certificate(cert_info.0, cert_info.1);
        }
        if let Some(stake_info) = stake_info {
            config.set_staked_nodes(stake_info.0, stake_info.1);
        }
        let connection_manager = QuicConnectionManager::new_with_connection_config(config);
        let cache =
            BackendConnectionCache::new(name, connection_manager, connection_pool_size).unwrap();
        Self::Quic(Arc::new(cache))
    }

    #[inline]
    #[allow(dead_code)]
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::Quic(_) => Protocol::QUIC,
        }
    }

    #[allow(dead_code)]
    pub fn use_quic(&self) -> bool {
        matches!(self, Self::Quic(_))
    }

    #[allow(dead_code)]
    pub fn get_connection(&self, addr: &SocketAddr) -> BlockingClientConnection {
        match self {
            Self::Quic(cache) => BlockingClientConnection::Quic(cache.get_connection(addr)),
        }
    }

    #[allow(dead_code)]
    pub fn get_nonblocking_connection(&self, addr: &SocketAddr) -> NonblockingClientConnection {
        match self {
            Self::Quic(cache) => {
                NonblockingClientConnection::Quic(cache.get_nonblocking_connection(addr))
            }
        }
    }
}

macro_rules! dispatch {
    ($(#[$meta:meta])* $vis:vis fn $name:ident$(<$($t:ident: $cons:ident + ?Sized),*>)?(&self $(, $arg:ident: $ty:ty)*) $(-> $out:ty)?) => {
        #[inline]
        $(#[$meta])*
        $vis fn $name$(<$($t: $cons + ?Sized),*>)?(&self $(, $arg:$ty)*) $(-> $out)? {
            match self {
                Self::Quic(this) => this.$name($($arg, )*),
            }
        }
    };
    ($(#[$meta:meta])* $vis:vis fn $name:ident$(<$($t:ident: $cons:ident + ?Sized),*>)?(&mut self $(, $arg:ident: $ty:ty)*) $(-> $out:ty)?) => {
        #[inline]
        $(#[$meta])*
        $vis fn $name$(<$($t: $cons + ?Sized),*>)?(&mut self $(, $arg:$ty)*) $(-> $out)? {
            match self {
                Self::Quic(this) => this.$name($($arg, )*),
                Self::Udp(this) => this.$name($($arg, )*),
            }
        }
    };
}

pub(crate) use dispatch;

impl ClientConnection for BlockingClientConnection {
    dispatch!(fn server_addr(&self) -> &SocketAddr);
    dispatch!(fn send_data(&self, buffer: &[u8]) -> TransportResult<()>);
    dispatch!(fn send_data_async(&self, buffer: Vec<u8>) -> TransportResult<()>);
    dispatch!(fn send_data_batch(&self, buffers: &[Vec<u8>]) -> TransportResult<()>);
    dispatch!(fn send_data_batch_async(&self, buffers: Vec<Vec<u8>>) -> TransportResult<()>);
}

#[async_trait::async_trait]
impl solana_connection_cache::nonblocking::client_connection::ClientConnection
    for NonblockingClientConnection
{
    dispatch!(fn server_addr(&self) -> &SocketAddr);

    async fn send_data(&self, buffer: &[u8]) -> TransportResult<()> {
        match self {
            Self::Quic(cache) => Ok(cache.send_data(buffer).await?),
        }
    }

    async fn send_data_batch(&self, buffers: &[Vec<u8>]) -> TransportResult<()> {
        match self {
            Self::Quic(cache) => Ok(cache.send_data_batch(buffers).await?),
        }
    }
}
