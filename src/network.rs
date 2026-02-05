use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};

pub trait DataPlane: Send + Sync {
    fn broadcast(&self, data: Vec<u8>) -> Result<(), String>;
    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>;
}

pub struct InMemoryDataPlane {
    subscribers: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
}

impl InMemoryDataPlane {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl DataPlane for InMemoryDataPlane {
    fn broadcast(&self, data: Vec<u8>) -> Result<(), String> {
        let subscribers = self.subscribers.lock();
        for sender in subscribers.iter() {
            let _ = sender.send(data.clone());
        }
        Ok(())
    }

    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.subscribers.lock().push(tx);
        rx
    }
}

// Shared network for connecting multiple nodes in tests
pub struct SharedNetwork {
    subscribers: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
}

impl SharedNetwork {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn create_data_plane(&self) -> Arc<SharedDataPlane> {
        Arc::new(SharedDataPlane {
            network: self.subscribers.clone(),
        })
    }
}

// Symbol-level network for routing Raptorcast symbols between nodes
pub struct SymbolNetwork {
    symbol_subscribers:
        Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<crate::raptorcast::Symbol>>>>,
}

impl SymbolNetwork {
    pub fn new() -> Self {
        Self {
            symbol_subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a Raptorcast data plane connected to this symbol network
    pub fn create_raptorcast_data_plane(
        &self,
    ) -> (Arc<crate::raptorcast::Raptorcast>, Arc<RaptorcastDataPlane>) {
        use crate::raptorcast::{Raptorcast, RaptorcastConfig};
        use tokio::sync::mpsc;

        let config = RaptorcastConfig::default();
        let raptorcast = Arc::new(Raptorcast::new(config));

        // Create symbol channel for receiving symbols from this Raptorcast instance
        let (symbol_tx, mut symbol_rx) = mpsc::unbounded_channel();

        // Set symbol sender in Raptorcast (this is where it will send generated symbols)
        raptorcast.set_symbol_sender(symbol_tx);

        // Create a receiver channel for symbols from other nodes
        let (rx_tx, rx_rx) = mpsc::unbounded_channel();

        {
            let mut subs = self.symbol_subscribers.lock();
            subs.push(rx_tx);
        }

        // Create data plane
        let data_plane = Arc::new(RaptorcastDataPlane::new(raptorcast.clone(), rx_rx));

        // Set up symbol routing: when this Raptorcast generates symbols, broadcast them to ALL nodes
        // (including ourselves, so we can test the full encoding/decoding path)
        let symbol_network = self.symbol_subscribers.clone();
        tokio::spawn(async move {
            while let Some(symbol) = symbol_rx.recv().await {
                // Broadcast symbol to all nodes (including ourselves for full test coverage)
                let subscribers = symbol_network.lock();
                for sender in subscribers.iter() {
                    let _ = sender.send(symbol.clone());
                }
            }
        });

        (raptorcast, data_plane)
    }

    /// Broadcast a symbol to all connected nodes
    pub fn broadcast_symbol(&self, symbol: crate::raptorcast::Symbol) {
        let subscribers = self.symbol_subscribers.lock();
        for sender in subscribers.iter() {
            let _ = sender.send(symbol.clone());
        }
    }
}

pub struct SharedDataPlane {
    network: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
}

impl DataPlane for SharedDataPlane {
    fn broadcast(&self, data: Vec<u8>) -> Result<(), String> {
        let subscribers = self.network.lock();
        for sender in subscribers.iter() {
            let _ = sender.send(data.clone());
        }
        Ok(())
    }

    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.network.lock().push(tx);
        rx
    }
}

// Raptorcast-based DataPlane implementation using our custom raptorcast module
pub struct RaptorcastDataPlane {
    raptorcast: Arc<crate::raptorcast::Raptorcast>,
    subscribers: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    symbol_network: Option<Arc<SymbolNetwork>>,
}

impl RaptorcastDataPlane {
    pub fn new(
        raptorcast: Arc<crate::raptorcast::Raptorcast>,
        symbol_receiver: mpsc::UnboundedReceiver<crate::raptorcast::Symbol>,
    ) -> Self {
        let subscribers: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let subscribers_clone = subscribers.clone();
        let raptorcast_clone = raptorcast.clone();

        // Spawn task to forward decoded messages from raptorcast to subscribers
        tokio::spawn(async move {
            let mut receiver = raptorcast_clone.subscribe();
            while let Some(message) = receiver.recv().await {
                let subs = subscribers_clone.lock();
                for sender in subs.iter() {
                    let _ = sender.send(message.clone());
                }
            }
        });

        // Spawn task to handle symbols from the network (from other nodes)
        let raptorcast_for_symbols = raptorcast.clone();
        tokio::spawn(async move {
            let mut receiver = symbol_receiver;
            while let Some(symbol) = receiver.recv().await {
                // Receive and decode symbol from another node
                raptorcast_for_symbols.receive_symbol(symbol);
            }
        });

        Self {
            raptorcast,
            subscribers,
            symbol_network: None,
        }
    }

    pub fn with_symbol_network(mut self, network: Arc<SymbolNetwork>) -> Self {
        self.symbol_network = Some(network);
        self
    }
}

impl DataPlane for RaptorcastDataPlane {
    fn broadcast(&self, data: Vec<u8>) -> Result<(), String> {
        self.raptorcast.broadcast(data)
    }

    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.subscribers.lock().push(tx);
        rx
    }
}

/// Simple TCP-based DataPlane for multi-node deployments.
///
/// - Each node runs a listener on `listen_addr`.
/// - Each node also connects out to a set of `peer_addrs`.
/// - Messages are framed as: 4-byte big-endian length prefix + payload bytes.
pub struct TcpDataPlane {
    writers: Arc<Mutex<Vec<mpsc::UnboundedSender<Vec<u8>>>>>,
    subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<Vec<u8>>>>>,
}

impl TcpDataPlane {
    /// Create a new TCP data plane.
    ///
    /// - `listen_addr`: local TCP address to listen on (e.g. "0.0.0.0:9000").
    /// - `peer_addrs`: list of other node addresses (e.g. ["orion-bench2:9000", "orion-bench3:9000"]).
    pub async fn new(listen_addr: String, peer_addrs: Vec<String>) -> Arc<Self> {
        let dp = Arc::new(Self {
            writers: Arc::new(Mutex::new(Vec::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
        });

        // Start listener for inbound connections.
        {
            let dp_clone = dp.clone();
            tokio::spawn(async move {
                let listener = match TcpListener::bind(&listen_addr).await {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("TcpDataPlane: failed to bind {}: {}", listen_addr, e);
                        return;
                    }
                };
                loop {
                    match listener.accept().await {
                        Ok((mut socket, _)) => {
                            let subscribers = dp_clone.subscribers.clone();
                            tokio::spawn(async move {
                                let mut len_buf = [0u8; 4];
                                loop {
                                    if socket.read_exact(&mut len_buf).await.is_err() {
                                        break;
                                    }
                                    let len = u32::from_be_bytes(len_buf) as usize;
                                    let mut buf = vec![0u8; len];
                                    if socket.read_exact(&mut buf).await.is_err() {
                                        break;
                                    }
                                    let subs = subscribers.lock();
                                    for s in subs.iter() {
                                        let _ = s.send(buf.clone());
                                    }
                                }
                            });
                        }
                        Err(_e) => {
                            // transient accept error; back off a bit
                            sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            });
        }

        // Outbound connections to peers.
        for addr in peer_addrs {
            let writers = dp.writers.clone();
            tokio::spawn(async move {
                loop {
                    match TcpStream::connect(&addr).await {
                        Ok(mut stream) => {
                            let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                            {
                                writers.lock().push(tx.clone());
                            }
                            while let Some(msg) = rx.recv().await {
                                let len = (msg.len() as u32).to_be_bytes();
                                if stream.write_all(&len).await.is_err() {
                                    break;
                                }
                                if stream.write_all(&msg).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(_e) => {
                            // Retry periodically until the peer is up.
                            sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            });
        }

        dp
    }
}

impl DataPlane for TcpDataPlane {
    fn broadcast(&self, data: Vec<u8>) -> Result<(), String> {
        let writers = self.writers.lock();
        for w in writers.iter() {
            let _ = w.send(data.clone());
        }
        Ok(())
    }

    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.subscribers.lock().push(tx);
        rx
    }
}
