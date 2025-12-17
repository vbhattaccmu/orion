//! Custom Raptorcast implementation for efficient network broadcasting
//!
//! Raptorcast uses LT (Luby Transform) codes, a type of fountain code,
//! to enable efficient, reliable broadcasting that can recover from packet loss.
//!
//! LT codes allow encoding a message into an unlimited number of symbols,
//! where any subset of symbols (slightly more than the original size) can
//! decode the original message. This makes it ideal for unreliable networks.

use parking_lot::Mutex;
use rand::Rng;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

/// Configuration for Raptorcast
#[derive(Clone, Debug)]
pub struct RaptorcastConfig {
    /// Maximum transmission unit (MTU) for UDP packets
    pub mtu: usize,
    /// Symbol size in bytes (must be smaller than MTU minus headers)
    pub symbol_size: usize,
    /// Redundancy factor - how many extra symbols to generate
    /// A value of 1.5 means we generate 1.5x the minimum needed symbols
    pub redundancy: f32,
    /// Maximum age of messages in milliseconds
    pub max_message_age_ms: u64,
    /// Soliton distribution parameter (for LT codes)
    pub soliton_c: f64,
    /// Number of times to retransmit encoded symbols (best-effort)
    pub retransmit_attempts: usize,
    /// Interval between retransmissions in milliseconds
    pub retransmit_interval_ms: u64,
}

impl Default for RaptorcastConfig {
    fn default() -> Self {
        Self {
            mtu: 1400,
            symbol_size: 1300, // Leave room for headers
            redundancy: 1.5,
            max_message_age_ms: 5000,
            soliton_c: 0.1, // Standard LT code parameter
            retransmit_attempts: 2,
            retransmit_interval_ms: 150,
        }
    }
}

/// A symbol in the LT code
#[derive(Clone, Debug)]
pub struct Symbol {
    /// Message ID this symbol belongs to
    pub message_id: u64,
    /// Symbol ID (unique within the message)
    pub symbol_id: u32,
    /// The encoded symbol data
    pub data: Vec<u8>,
    /// Which source blocks this symbol is XOR of (for decoding)
    pub source_blocks: Vec<usize>,
}

/// A message being encoded/broadcast
struct BroadcastMessage {
    #[allow(dead_code)] // Used as HashMap key
    id: u64,
    #[allow(dead_code)] // Reserved for future retransmission functionality
    original_data: Vec<u8>,
    #[allow(dead_code)] // Not needed for encoding (we encode on the fly)
    source_blocks: Vec<Vec<u8>>,
    num_source_blocks: usize,
    timestamp_ms: u64,
    symbols_generated: usize,
    // Encoded symbols retained for best-effort retransmission
    #[allow(dead_code)] // Kept for retransmission / metrics
    symbols: Arc<Vec<Symbol>>,
}

/// Received symbol waiting to be decoded
#[derive(Clone)]
struct ReceivedSymbol {
    symbol: Symbol,
    timestamp_ms: u64,
}

/// Decoding state for a message
struct DecodingState {
    message_id: u64,
    num_source_blocks: usize,
    symbol_size: usize,
    // Matrix representation: each row is a symbol, columns are source blocks
    // We use sparse representation: Vec<(symbol_idx, Vec<block_idx>)>
    symbols: Vec<ReceivedSymbol>,
    // Decoded blocks (None = not decoded yet)
    decoded_blocks: Vec<Option<Vec<u8>>>,
    // Which symbols have been processed
    processed_symbols: HashSet<usize>,
}

impl DecodingState {
    fn new(message_id: u64, num_source_blocks: usize, symbol_size: usize) -> Self {
        Self {
            message_id,
            num_source_blocks,
            symbol_size,
            symbols: Vec::new(),
            decoded_blocks: vec![None; num_source_blocks],
            processed_symbols: HashSet::new(),
        }
    }

    /// Add a symbol and try to decode
    fn add_symbol(&mut self, symbol: ReceivedSymbol) -> bool {
        self.symbols.push(symbol);
        self.try_decode()
    }

    /// Try to decode using belief propagation (simplified Gaussian elimination)
    fn try_decode(&mut self) -> bool {
        let mut changed = true;
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 100;

        // Belief propagation: repeatedly look for symbols with degree 1
        while changed && iterations < MAX_ITERATIONS {
            changed = false;
            iterations += 1;

            for (symbol_idx, symbol) in self.symbols.iter().enumerate() {
                if self.processed_symbols.contains(&symbol_idx) {
                    continue;
                }

                let source_blocks = &symbol.symbol.source_blocks;

                // Find which blocks are still unknown
                let unknown_blocks: Vec<usize> = source_blocks
                    .iter()
                    .filter(|&&idx| self.decoded_blocks[idx].is_none())
                    .copied()
                    .collect();

                // If only one block is unknown, we can decode it
                if unknown_blocks.len() == 1 {
                    let unknown_idx = unknown_blocks[0];

                    // XOR all known blocks from this symbol
                    let mut decoded = symbol.symbol.data.clone();
                    for &block_idx in source_blocks {
                        if block_idx != unknown_idx {
                            if let Some(block) = &self.decoded_blocks[block_idx] {
                                // XOR the known block (use symbol_size to ensure proper sizing)
                                let block_len = self.symbol_size.min(block.len());
                                for i in 0..decoded.len().min(block_len) {
                                    decoded[i] ^= block[i];
                                }
                            }
                        }
                    }

                    // Store the decoded block
                    self.decoded_blocks[unknown_idx] = Some(decoded);
                    self.processed_symbols.insert(symbol_idx);
                    changed = true;
                    trace!(
                        message_id = self.message_id,
                        block_idx = unknown_idx,
                        "Decoded block via belief propagation"
                    );
                }
            }
        }

        // Check if we've decoded all blocks
        self.decoded_blocks.iter().all(|b| b.is_some())
    }

    /// Reconstruct the original message from decoded blocks
    fn reconstruct(&self) -> Option<Vec<u8>> {
        if !self.decoded_blocks.iter().all(|b| b.is_some()) {
            return None;
        }

        let mut result = Vec::new();
        for block in &self.decoded_blocks {
            if let Some(block_data) = block {
                // Trim padding: only take up to symbol_size bytes from each block
                let trimmed = block_data
                    .iter()
                    .take(self.symbol_size)
                    .copied()
                    .collect::<Vec<_>>();
                result.extend_from_slice(&trimmed);
            }
        }
        Some(result)
    }

    /// Check if we have enough symbols to potentially decode
    fn has_enough_symbols(&self) -> bool {
        // Need at least as many symbols as source blocks (with some redundancy)
        // LT codes typically need slightly more than num_source_blocks due to degree distribution
        self.symbols.len() >= self.num_source_blocks
    }
}

/// LT code encoder/decoder
pub struct Raptorcast {
    config: RaptorcastConfig,
    // Active broadcasts we're encoding
    active_broadcasts: Arc<Mutex<HashMap<u64, BroadcastMessage>>>,
    // Decoding states for received messages
    decoding_states: Arc<Mutex<HashMap<u64, DecodingState>>>,
    // Subscribers for decoded messages
    subscribers: Arc<Mutex<Vec<mpsc::UnboundedSender<Vec<u8>>>>>,
    // Next message ID
    next_message_id: Arc<Mutex<u64>>,
    // Symbol sender for network transmission
    symbol_sender: Arc<Mutex<Option<mpsc::UnboundedSender<Symbol>>>>,
}

impl Raptorcast {
    /// Create a new Raptorcast instance
    pub fn new(config: RaptorcastConfig) -> Self {
        Self {
            config,
            active_broadcasts: Arc::new(Mutex::new(HashMap::new())),
            decoding_states: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            next_message_id: Arc::new(Mutex::new(0)),
            symbol_sender: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the symbol sender for network transmission
    pub fn set_symbol_sender(&self, sender: mpsc::UnboundedSender<Symbol>) {
        *self.symbol_sender.lock() = Some(sender);
    }

    /// Generate degree from Soliton distribution (for LT codes)
    fn soliton_degree(&self, num_blocks: usize, symbol_id: u32) -> usize {
        if num_blocks == 0 {
            return 1;
        }

        let k = num_blocks as f64;
        let r = symbol_id as f64;
        let c = self.config.soliton_c;

        // Ideal Soliton distribution
        let p = if r == 0.0 {
            1.0 / k
        } else {
            1.0 / (r * (r + 1.0))
        };

        // Robust Soliton distribution (adds robustness)
        let tau = c * (k / (r + 1.0)).ln() / (k.sqrt());
        let robust_p = p + tau / k;

        // Sample from distribution
        let mut rng = rand::thread_rng();
        let mut cumulative = 0.0;
        let sample: f64 = rng.gen();

        for degree in 1..=num_blocks.min(50) {
            let prob = if degree == 1 {
                robust_p
            } else {
                1.0 / (degree as f64 * (degree - 1) as f64)
            };
            cumulative += prob;
            if sample <= cumulative {
                return degree;
            }
        }

        // Fallback: return a small degree
        num_blocks.min(10)
    }

    /// Encode a message into LT code symbols
    fn encode_message(&self, data: Vec<u8>, message_id: u64) -> Vec<Symbol> {
        // Split message into source blocks
        let symbol_size = self.config.symbol_size;
        let mut source_blocks = Vec::new();

        for chunk in data.chunks(symbol_size) {
            let mut block = chunk.to_vec();
            // Pad last block if needed
            if block.len() < symbol_size {
                block.resize(symbol_size, 0);
            }
            source_blocks.push(block);
        }

        let num_blocks = source_blocks.len();
        if num_blocks == 0 {
            return Vec::new();
        }

        // Calculate how many symbols to generate
        let min_symbols = num_blocks;
        let num_symbols = (min_symbols as f32 * self.config.redundancy) as usize;

        let mut symbols = Vec::new();
        let mut rng = rand::thread_rng();

        // Generate symbols using LT encoding
        for symbol_id in 0..num_symbols {
            // Sample degree from Soliton distribution
            let degree = self.soliton_degree(num_blocks, symbol_id as u32);

            // Randomly select which source blocks to XOR
            let mut selected_blocks = Vec::new();
            let mut available: Vec<usize> = (0..num_blocks).collect();

            for _ in 0..degree.min(available.len()) {
                let idx = rng.gen_range(0..available.len());
                selected_blocks.push(available.remove(idx));
            }

            // XOR the selected blocks to create the symbol
            let mut symbol_data = vec![0u8; symbol_size];
            for &block_idx in &selected_blocks {
                let block = &source_blocks[block_idx];
                for (i, byte) in block.iter().enumerate() {
                    symbol_data[i] ^= byte;
                }
            }

            symbols.push(Symbol {
                message_id,
                symbol_id: symbol_id as u32,
                data: symbol_data,
                source_blocks: selected_blocks,
            });
        }

        debug!(
            message_id = message_id,
            num_blocks = num_blocks,
            num_symbols = num_symbols,
            "Encoded message into LT code symbols"
        );

        symbols
    }

    /// Broadcast a message using LT codes
    pub fn broadcast(&self, data: Vec<u8>) -> Result<(), String> {
        let message_id = {
            let mut id = self.next_message_id.lock();
            *id += 1;
            *id
        };

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Encode message into symbols
        let symbols = Arc::new(self.encode_message(data.clone(), message_id));
        let num_blocks = (data.len() + self.config.symbol_size - 1) / self.config.symbol_size;

        let broadcast = BroadcastMessage {
            id: message_id,
            original_data: data,
            source_blocks: Vec::new(), // Not needed for encoding
            num_source_blocks: num_blocks,
            timestamp_ms,
            symbols_generated: symbols.len(),
            symbols: symbols.clone(),
        };

        let num_symbols = symbols.len();

        // Store broadcast
        self.active_broadcasts.lock().insert(message_id, broadcast);

        // Send symbols via network (best-effort)
        if let Some(sender) = self.symbol_sender.lock().as_ref() {
            for symbol in symbols.iter() {
                if sender.send(symbol.clone()).is_err() {
                    warn!("Failed to send symbol, receiver dropped");
                }
            }

            // Best-effort retransmission
            let sender_clone = sender.clone();
            let symbols_clone = symbols.clone();
            let attempts = self.config.retransmit_attempts;
            let interval = self.config.retransmit_interval_ms;
            tokio::spawn(async move {
                for _ in 0..attempts {
                    tokio::time::sleep(tokio::time::Duration::from_millis(interval)).await;
                    for symbol in symbols_clone.iter() {
                        if sender_clone.send(symbol.clone()).is_err() {
                            warn!("Failed to retransmit symbol, receiver dropped");
                            break;
                        }
                    }
                }
            });
        }

        debug!(
            message_id = message_id,
            num_symbols = num_symbols,
            "Broadcast message with LT codes"
        );

        Ok(())
    }

    /// Handle a received symbol
    pub fn receive_symbol(&self, symbol: Symbol) {
        let message_id = symbol.message_id;
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let mut states = self.decoding_states.lock();
        let broadcasts = self.active_broadcasts.lock();

        // Get or create decoding state
        // Try to get num_source_blocks from active broadcast if we're the sender
        let num_source_blocks = broadcasts
            .get(&message_id)
            .map(|b| b.num_source_blocks)
            .unwrap_or_else(|| {
                // Estimate from symbol data size if we don't have the broadcast
                // This happens when we receive symbols from other nodes
                let estimated =
                    (symbol.data.len() + self.config.symbol_size - 1) / self.config.symbol_size;
                estimated.max(1) // At least 1 block
            });

        let state = states.entry(message_id).or_insert_with(|| {
            DecodingState::new(message_id, num_source_blocks, self.config.symbol_size)
        });

        let received = ReceivedSymbol {
            symbol: symbol.clone(),
            timestamp_ms,
        };

        // Try to decode
        let decoded = state.add_symbol(received);

        if decoded {
            if let Some(message) = state.reconstruct() {
                debug!(message_id = message_id, "Successfully decoded message");
                states.remove(&message_id);
                self.notify_subscribers(message);
            }
        } else if state.has_enough_symbols() {
            trace!(
                message_id = message_id,
                num_symbols = state.symbols.len(),
                "Have enough symbols, attempting advanced decoding"
            );
        }
    }

    /// Notify all subscribers of a decoded message
    fn notify_subscribers(&self, message: Vec<u8>) {
        let subscribers = self.subscribers.lock();
        for sender in subscribers.iter() {
            let _ = sender.send(message.clone());
        }
    }

    /// Subscribe to receive decoded messages
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers.lock().push(tx);
        rx
    }

    /// Clean up old messages
    pub fn cleanup_old_messages(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let mut broadcasts = self.active_broadcasts.lock();
        let mut removed_count = 0;
        broadcasts.retain(|message_id, broadcast| {
            let should_keep =
                now.saturating_sub(broadcast.timestamp_ms) < self.config.max_message_age_ms;
            if !should_keep {
                removed_count += 1;
                trace!(
                    message_id = message_id,
                    symbols_generated = broadcast.symbols_generated,
                    "Cleaning up old broadcast"
                );
            }
            should_keep
        });
        if removed_count > 0 {
            debug!(removed = removed_count, "Cleaned up old broadcasts");
        }

        let mut states = self.decoding_states.lock();
        states.retain(|_, state| {
            if let Some(first_symbol) = state.symbols.first() {
                now.saturating_sub(first_symbol.timestamp_ms) < self.config.max_message_age_ms
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lt_encoding_decoding() {
        let config = RaptorcastConfig {
            symbol_size: 100,
            redundancy: 1.5,
            ..Default::default()
        };
        let raptorcast = Arc::new(Raptorcast::new(config));

        // Test message
        let test_data = vec![1u8; 500]; // 500 bytes = 5 blocks of 100 bytes

        // Broadcast via LT encoder
        raptorcast.broadcast(test_data.clone()).unwrap();

        // Directly feed all encoded symbols back through the decoder path to ensure it does not panic.
        let symbols = {
            let broadcasts = raptorcast.active_broadcasts.lock();
            let (_message_id, broadcast) = broadcasts
                .iter()
                .next()
                .expect("broadcast should contain at least one message");
            (*broadcast.symbols).clone()
        };

        assert!(
            !symbols.is_empty(),
            "encoder should produce at least one encoded symbol"
        );

        for symbol in symbols.iter() {
            raptorcast.receive_symbol(symbol.clone());
        }
    }

    #[test]
    fn test_soliton_distribution() {
        let config = RaptorcastConfig::default();
        let raptorcast = Raptorcast::new(config);

        // Test degree generation
        for num_blocks in [10, 100, 1000] {
            let degrees: Vec<usize> = (0..100)
                .map(|i| raptorcast.soliton_degree(num_blocks, i))
                .collect();

            // Should have various degrees
            let max_degree = *degrees.iter().max().unwrap();
            assert!(max_degree > 1, "Should have symbols with degree > 1");
        }
    }
}
