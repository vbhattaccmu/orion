use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::mpsc;

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
