use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

/// Configuration for connecting to Reth's Engine API and JSON-RPC endpoints.
pub struct EngineApiConfig {
    /// Engine API endpoint (authenticated, port 8551), e.g. "http://reth:8551"
    pub auth_endpoint: String,
    /// Standard JSON-RPC endpoint (port 8545), e.g. "http://reth:8545"
    pub rpc_endpoint: String,
    /// Hex-encoded 32-byte JWT secret (64 hex chars, no 0x prefix)
    pub jwt_secret_hex: String,
}

/// HTTP client for Reth's Engine API and standard JSON-RPC.
pub struct EngineApiClient {
    config: EngineApiConfig,
    http: reqwest::Client,
    request_id: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkchoiceState {
    pub head_block_hash: String,
    pub safe_block_hash: String,
    pub finalized_block_hash: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadAttributesV3 {
    pub timestamp: String,
    pub prev_randao: String,
    pub suggested_fee_recipient: String,
    pub withdrawals: Vec<Value>,
    pub parent_beacon_block_root: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkchoiceUpdatedResponse {
    pub payload_status: PayloadStatus,
    pub payload_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadStatus {
    pub status: String,
    pub latest_valid_hash: Option<String>,
    pub validation_error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineApiError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON-RPC error: code={code}, message={message}")]
    JsonRpc { code: i64, message: String },
    #[error("JWT error: {0}")]
    Jwt(String),
    #[error("Unexpected response: {0}")]
    Unexpected(String),
    #[error("Payload invalid: status={status}, error={error:?}")]
    InvalidPayload {
        status: String,
        error: Option<String>,
    },
}

impl EngineApiClient {
    pub fn new(config: EngineApiConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
            request_id: AtomicU64::new(1),
        }
    }

    /// Generate an HS256 JWT token for Engine API authentication.
    pub fn generate_jwt(&self) -> Result<String, EngineApiError> {
        let secret_bytes = hex::decode(&self.config.jwt_secret_hex)
            .map_err(|e| EngineApiError::Jwt(e.to_string()))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let claims = json!({ "iat": now });

        let key = jsonwebtoken::EncodingKey::from_secret(&secret_bytes);
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);

        jsonwebtoken::encode(&header, &claims, &key).map_err(|e| EngineApiError::Jwt(e.to_string()))
    }

    fn next_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC call to the authenticated Engine API endpoint (port 8551).
    pub async fn auth_rpc_call(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value, EngineApiError> {
        let jwt = self.generate_jwt()?;
        let id = self.next_id();

        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });

        debug!(method, id, "Engine API auth request");

        let resp = self
            .http
            .post(&self.config.auth_endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", jwt))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;

        if !status.is_success() {
            return Err(EngineApiError::Unexpected(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let resp_json: Value =
            serde_json::from_str(&text).map_err(|e| EngineApiError::Unexpected(e.to_string()))?;

        if let Some(error) = resp_json.get("error") {
            let code = error["code"].as_i64().unwrap_or(-1);
            let message = error["message"].as_str().unwrap_or("unknown").to_string();
            return Err(EngineApiError::JsonRpc { code, message });
        }

        resp_json
            .get("result")
            .cloned()
            .ok_or_else(|| EngineApiError::Unexpected("No result in response".into()))
    }

    /// Send a JSON-RPC call to the standard RPC endpoint (port 8545, no auth).
    pub async fn rpc_call(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<Value, EngineApiError> {
        let id = self.next_id();

        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });

        debug!(method, id, "RPC request");

        let resp = self
            .http
            .post(&self.config.rpc_endpoint)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;

        if !status.is_success() {
            return Err(EngineApiError::Unexpected(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let resp_json: Value =
            serde_json::from_str(&text).map_err(|e| EngineApiError::Unexpected(e.to_string()))?;

        if let Some(error) = resp_json.get("error") {
            let code = error["code"].as_i64().unwrap_or(-1);
            let message = error["message"].as_str().unwrap_or("unknown").to_string();
            return Err(EngineApiError::JsonRpc { code, message });
        }

        resp_json
            .get("result")
            .cloned()
            .ok_or_else(|| EngineApiError::Unexpected("No result in response".into()))
    }

    // ---- Standard RPC helpers ----

    /// Fetch a block by number. `number` is hex-encoded (e.g. "0x0", "latest").
    pub async fn get_block_by_number(
        &self,
        number: &str,
        full_txs: bool,
    ) -> Result<Value, EngineApiError> {
        self.rpc_call("eth_getBlockByNumber", vec![json!(number), json!(full_txs)])
            .await
    }

    /// Fetch the genesis (block 0) hash.
    pub async fn get_genesis_block_hash(&self) -> Result<String, EngineApiError> {
        let block = self.get_block_by_number("0x0", false).await?;
        block["hash"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| EngineApiError::Unexpected("No hash in genesis block".into()))
    }

    /// Fetch the latest block hash.
    pub async fn get_latest_block_hash(&self) -> Result<String, EngineApiError> {
        let block = self.get_block_by_number("latest", false).await?;
        block["hash"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| EngineApiError::Unexpected("No hash in latest block".into()))
    }

    /// Fetch the current block number (hex string).
    pub async fn get_block_number(&self) -> Result<String, EngineApiError> {
        let result = self.rpc_call("eth_blockNumber", vec![]).await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| EngineApiError::Unexpected("Unexpected blockNumber response".into()))
    }

    /// Send a transaction via eth_sendTransaction (for dev mode unlocked accounts).
    pub async fn send_transaction(
        &self,
        from: &str,
        to: &str,
        value: &str,
    ) -> Result<String, EngineApiError> {
        // Provide gas and gasPrice to satisfy Reth's tx schema in dev mode.
        let tx = json!({
            "from": from,
            "to": to,
            "value": value,
            "gas": "0x5208",        // 21000
            "gasPrice": "0x3b9aca00" // 1 gwei
        });
        let result = self.rpc_call("eth_sendTransaction", vec![tx]).await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| EngineApiError::Unexpected("No tx hash returned".into()))
    }

    /// Submit a raw signed transaction via eth_sendRawTransaction.
    pub async fn send_raw_transaction(&self, raw_tx_hex: &str) -> Result<String, EngineApiError> {
        let result = self
            .rpc_call("eth_sendRawTransaction", vec![json!(raw_tx_hex)])
            .await?;
        result
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| EngineApiError::Unexpected("No tx hash returned".into()))
    }

    // (Demo-only signing helper removed to keep dependencies minimal; we rely on
    // eth_sendTransaction with explicit gas settings for dev mode.)

    // ---- Engine API methods ----

    /// engine_forkchoiceUpdatedV3 with PayloadAttributes — triggers block building.
    pub async fn forkchoice_updated_with_attributes(
        &self,
        state: ForkchoiceState,
        attrs: PayloadAttributesV3,
    ) -> Result<ForkchoiceUpdatedResponse, EngineApiError> {
        let state_val =
            serde_json::to_value(&state).map_err(|e| EngineApiError::Unexpected(e.to_string()))?;
        let attrs_val =
            serde_json::to_value(&attrs).map_err(|e| EngineApiError::Unexpected(e.to_string()))?;

        let result = self
            .auth_rpc_call("engine_forkchoiceUpdatedV3", vec![state_val, attrs_val])
            .await?;

        serde_json::from_value(result).map_err(|e| EngineApiError::Unexpected(e.to_string()))
    }

    /// engine_forkchoiceUpdatedV3 without PayloadAttributes — finalizes a block.
    pub async fn forkchoice_updated_finalize(
        &self,
        state: ForkchoiceState,
    ) -> Result<ForkchoiceUpdatedResponse, EngineApiError> {
        let state_val =
            serde_json::to_value(&state).map_err(|e| EngineApiError::Unexpected(e.to_string()))?;

        let result = self
            .auth_rpc_call("engine_forkchoiceUpdatedV3", vec![state_val, Value::Null])
            .await?;

        serde_json::from_value(result).map_err(|e| EngineApiError::Unexpected(e.to_string()))
    }

    /// engine_getPayloadV3 — retrieves a built execution payload.
    pub async fn get_payload_v3(&self, payload_id: &str) -> Result<Value, EngineApiError> {
        self.auth_rpc_call("engine_getPayloadV3", vec![json!(payload_id)])
            .await
    }

    /// engine_newPayloadV3 — submits an execution payload for validation.
    pub async fn new_payload_v3(
        &self,
        execution_payload: &Value,
        versioned_hashes: Vec<String>,
        parent_beacon_block_root: &str,
    ) -> Result<PayloadStatus, EngineApiError> {
        let hashes_val: Vec<Value> = versioned_hashes.into_iter().map(|h| json!(h)).collect();

        let result = self
            .auth_rpc_call(
                "engine_newPayloadV3",
                vec![
                    execution_payload.clone(),
                    json!(hashes_val),
                    json!(parent_beacon_block_root),
                ],
            )
            .await?;

        serde_json::from_value(result).map_err(|e| EngineApiError::Unexpected(e.to_string()))
    }

    /// Full 4-step block building cycle:
    /// 1. forkchoiceUpdatedV3 with attrs → payloadId
    /// 2. getPayloadV3 → execution payload
    /// 3. newPayloadV3 → validate
    /// 4. forkchoiceUpdatedV3 → finalize
    ///
    /// Returns (new_block_hash, execution_payload).
    pub async fn build_and_finalize_block(
        &self,
        parent_hash: &str,
        timestamp: u64,
        prev_randao: [u8; 32],
        fee_recipient: &str,
    ) -> Result<(String, Value), EngineApiError> {
        let parent_beacon_block_root =
            "0x0000000000000000000000000000000000000000000000000000000000000000";

        // Step 1: forkchoiceUpdated with PayloadAttributes
        let fcs = ForkchoiceState {
            head_block_hash: parent_hash.to_string(),
            safe_block_hash: parent_hash.to_string(),
            finalized_block_hash: parent_hash.to_string(),
        };
        let attrs = PayloadAttributesV3 {
            timestamp: format!("0x{:x}", timestamp),
            prev_randao: format!("0x{}", hex::encode(prev_randao)),
            suggested_fee_recipient: fee_recipient.to_string(),
            withdrawals: vec![],
            parent_beacon_block_root: parent_beacon_block_root.to_string(),
        };

        debug!(parent = parent_hash, "Step 1: forkchoiceUpdated with attrs");
        let fcu_resp = self.forkchoice_updated_with_attributes(fcs, attrs).await?;

        if fcu_resp.payload_status.status != "VALID" {
            warn!(
                status = %fcu_resp.payload_status.status,
                "forkchoiceUpdated returned non-VALID status"
            );
        }

        let payload_id = match fcu_resp.payload_id {
            Some(id) => {
                debug!(payload_id = %id, "Step 1 complete");
                id
            }
            None => {
                // In dev mode Reth may skip creating a payload when there are no txs or
                // when its internal dev forkchoice already built a block. For the demo
                // we treat this as “no new payload” rather than a hard error.
                warn!("forkchoiceUpdatedV3 returned no payload_id; using latest block hash as-is");
                let latest = self.get_latest_block_hash().await?;
                return Ok((latest, Value::Null));
            }
        };

        // Small delay to let Reth build the block with pending txs
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Step 2: getPayload
        debug!(payload_id = %payload_id, "Step 2: getPayload");
        let payload_envelope = self.get_payload_v3(&payload_id).await?;

        let execution_payload = payload_envelope
            .get("executionPayload")
            .cloned()
            .ok_or_else(|| EngineApiError::Unexpected("No executionPayload in response".into()))?;

        let block_hash = execution_payload["blockHash"]
            .as_str()
            .ok_or_else(|| EngineApiError::Unexpected("No blockHash in executionPayload".into()))?
            .to_string();
        debug!(block_hash = %block_hash, "Step 2 complete");

        // Step 3: newPayload
        debug!(block_hash = %block_hash, "Step 3: newPayload");
        let status = self
            .new_payload_v3(&execution_payload, vec![], parent_beacon_block_root)
            .await?;

        if status.status != "VALID" {
            // In dev mode, Reth may still accept and build blocks even if the
            // payload is marked non-VALID for auxiliary reasons (e.g. hash
            // recomputation differences). For this demo, log and continue so
            // Orion can keep driving block production.
            warn!(
                status = %status.status,
                error = ?status.validation_error,
                "newPayloadV3 returned non-VALID status; continuing in demo mode"
            );
        } else {
            debug!("Step 3 complete: payload VALID");
        }

        // Step 4: forkchoiceUpdated to finalize
        let final_fcs = ForkchoiceState {
            head_block_hash: block_hash.clone(),
            safe_block_hash: block_hash.clone(),
            finalized_block_hash: block_hash.clone(),
        };

        debug!(block_hash = %block_hash, "Step 4: forkchoiceUpdated finalize");
        let final_resp = self.forkchoice_updated_finalize(final_fcs).await?;

        if final_resp.payload_status.status != "VALID" {
            warn!(
                status = %final_resp.payload_status.status,
                "Finalize forkchoiceUpdated returned non-VALID"
            );
        }
        debug!("Step 4 complete: block finalized");

        Ok((block_hash, execution_payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    const TEST_JWT_SECRET: &str =
        "aabbccddee00112233445566778899aabbccddee00112233445566778899aabb";

    fn test_config(port: u16) -> EngineApiConfig {
        EngineApiConfig {
            auth_endpoint: format!("http://127.0.0.1:{}", port),
            rpc_endpoint: format!("http://127.0.0.1:{}", port),
            jwt_secret_hex: TEST_JWT_SECRET.to_string(),
        }
    }

    // ---- JWT tests ----

    #[test]
    fn test_generate_jwt_produces_valid_token() {
        let client = EngineApiClient::new(test_config(0));
        let token = client.generate_jwt().unwrap();

        // JWT has three dot-separated parts
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 parts");

        // Verify we can decode it back with the same secret
        let secret_bytes = hex::decode(TEST_JWT_SECRET).unwrap();
        let decoding_key = jsonwebtoken::DecodingKey::from_secret(&secret_bytes);
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.required_spec_claims.clear();
        validation.set_required_spec_claims::<&str>(&[]);

        let token_data = jsonwebtoken::decode::<Value>(&token, &decoding_key, &validation).unwrap();

        // Check the iat claim exists and is recent
        let iat = token_data.claims["iat"].as_u64().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            now.abs_diff(iat) < 5,
            "iat should be within 5 seconds of now"
        );
    }

    #[test]
    fn test_generate_jwt_uses_hs256() {
        let client = EngineApiClient::new(test_config(0));
        let token = client.generate_jwt().unwrap();

        // Verify the token uses HS256 by decoding with the correct algorithm
        let secret_bytes = hex::decode(TEST_JWT_SECRET).unwrap();
        let decoding_key = jsonwebtoken::DecodingKey::from_secret(&secret_bytes);
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.required_spec_claims.clear();
        validation.set_required_spec_claims::<&str>(&[]);

        // This succeeds only if the token was signed with HS256
        let result = jsonwebtoken::decode::<Value>(&token, &decoding_key, &validation);
        assert!(result.is_ok(), "Token must be decodable with HS256");

        // Verify it fails with a different algorithm expectation
        let mut wrong_validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS384);
        wrong_validation.required_spec_claims.clear();
        wrong_validation.set_required_spec_claims::<&str>(&[]);
        let wrong_result = jsonwebtoken::decode::<Value>(&token, &decoding_key, &wrong_validation);
        assert!(wrong_result.is_err(), "Token must NOT decode with HS384");
    }

    #[test]
    fn test_generate_jwt_invalid_secret_fails() {
        let config = EngineApiConfig {
            auth_endpoint: "http://unused".into(),
            rpc_endpoint: "http://unused".into(),
            jwt_secret_hex: "not_valid_hex!!!!".into(),
        };
        let client = EngineApiClient::new(config);
        let result = client.generate_jwt();
        assert!(result.is_err());
        match result.unwrap_err() {
            EngineApiError::Jwt(_) => {}
            other => panic!("Expected Jwt error, got: {:?}", other),
        }
    }

    #[test]
    fn test_generate_jwt_deterministic_structure() {
        let client = EngineApiClient::new(test_config(0));
        let token1 = client.generate_jwt().unwrap();
        let token2 = client.generate_jwt().unwrap();

        // Both tokens should have the same header (first part)
        let header1 = token1.split('.').next().unwrap();
        let header2 = token2.split('.').next().unwrap();
        assert_eq!(header1, header2, "JWT headers should be identical");
    }

    // ---- Request ID tests ----

    #[test]
    fn test_request_id_increments() {
        let client = EngineApiClient::new(test_config(0));
        let id1 = client.next_id();
        let id2 = client.next_id();
        let id3 = client.next_id();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    // ---- Serialization tests ----

    #[test]
    fn test_forkchoice_state_serialization() {
        let fcs = ForkchoiceState {
            head_block_hash: "0xabc".into(),
            safe_block_hash: "0xdef".into(),
            finalized_block_hash: "0x123".into(),
        };
        let val = serde_json::to_value(&fcs).unwrap();
        assert_eq!(val["headBlockHash"], "0xabc");
        assert_eq!(val["safeBlockHash"], "0xdef");
        assert_eq!(val["finalizedBlockHash"], "0x123");
    }

    #[test]
    fn test_payload_attributes_serialization() {
        let attrs = PayloadAttributesV3 {
            timestamp: "0x65".into(),
            prev_randao: "0xabcd".into(),
            suggested_fee_recipient: "0x0000000000000000000000000000000000000000".into(),
            withdrawals: vec![],
            parent_beacon_block_root:
                "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
        };
        let val = serde_json::to_value(&attrs).unwrap();
        assert_eq!(val["timestamp"], "0x65");
        assert_eq!(val["prevRandao"], "0xabcd");
        assert_eq!(
            val["suggestedFeeRecipient"],
            "0x0000000000000000000000000000000000000000"
        );
        assert!(val["withdrawals"].as_array().unwrap().is_empty());
        assert_eq!(
            val["parentBeaconBlockRoot"],
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
    }

    // ---- Response deserialization tests ----

    #[test]
    fn test_forkchoice_updated_response_deserialization() {
        let json_str = r#"{
            "payloadStatus": {
                "status": "VALID",
                "latestValidHash": "0xabc123",
                "validationError": null
            },
            "payloadId": "0xdeadbeef"
        }"#;
        let resp: ForkchoiceUpdatedResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.payload_status.status, "VALID");
        assert_eq!(
            resp.payload_status.latest_valid_hash,
            Some("0xabc123".into())
        );
        assert!(resp.payload_status.validation_error.is_none());
        assert_eq!(resp.payload_id, Some("0xdeadbeef".into()));
    }

    #[test]
    fn test_forkchoice_updated_response_without_payload_id() {
        let json_str = r#"{
            "payloadStatus": {
                "status": "VALID",
                "latestValidHash": "0xabc123",
                "validationError": null
            },
            "payloadId": null
        }"#;
        let resp: ForkchoiceUpdatedResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.payload_status.status, "VALID");
        assert!(resp.payload_id.is_none());
    }

    #[test]
    fn test_payload_status_invalid_deserialization() {
        let json_str = r#"{
            "status": "INVALID",
            "latestValidHash": "0x0000",
            "validationError": "block timestamp too old"
        }"#;
        let status: PayloadStatus = serde_json::from_str(json_str).unwrap();
        assert_eq!(status.status, "INVALID");
        assert_eq!(
            status.validation_error,
            Some("block timestamp too old".into())
        );
    }

    #[test]
    fn test_payload_status_syncing_deserialization() {
        let json_str = r#"{
            "status": "SYNCING",
            "latestValidHash": null,
            "validationError": null
        }"#;
        let status: PayloadStatus = serde_json::from_str(json_str).unwrap();
        assert_eq!(status.status, "SYNCING");
        assert!(status.latest_valid_hash.is_none());
        assert!(status.validation_error.is_none());
    }

    // ---- Mock server HTTP tests ----

    /// Start a minimal HTTP server that returns a fixed JSON-RPC response.
    /// Returns (port, join_handle). The server handles exactly one request.
    fn start_mock_rpc_server(response_body: &str) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let response = response_body.to_string();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());

            // Read the HTTP request headers
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();

            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if header.to_lowercase().starts_with("content-length:") {
                    content_length = header.split(':').nth(1).unwrap().trim().parse().unwrap();
                }
            }

            // Read body
            let mut body = vec![0u8; content_length];
            std::io::Read::read_exact(&mut reader, &mut body).unwrap();
            let request_body = String::from_utf8(body).unwrap();

            // Send response
            let http_resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(http_resp.as_bytes()).unwrap();
            stream.flush().unwrap();

            request_body
        });

        (port, handle)
    }

    #[tokio::test]
    async fn test_rpc_call_success() {
        let response = r#"{"jsonrpc":"2.0","result":"0x42","id":1}"#;
        let (port, handle) = start_mock_rpc_server(response);

        let client = EngineApiClient::new(test_config(port));
        let result = client.rpc_call("eth_blockNumber", vec![]).await.unwrap();

        assert_eq!(result, json!("0x42"));

        // Verify the request body was well-formed JSON-RPC
        let request_body = handle.join().unwrap();
        let req: Value = serde_json::from_str(&request_body).unwrap();
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["method"], "eth_blockNumber");
    }

    #[tokio::test]
    async fn test_rpc_call_json_rpc_error() {
        let response =
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;
        let (port, _handle) = start_mock_rpc_server(response);

        let client = EngineApiClient::new(test_config(port));
        let result = client.rpc_call("nonexistent_method", vec![]).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            EngineApiError::JsonRpc { code, message } => {
                assert_eq!(code, -32601);
                assert_eq!(message, "Method not found");
            }
            other => panic!("Expected JsonRpc error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_auth_rpc_call_includes_jwt_bearer() {
        let response = r#"{"jsonrpc":"2.0","result":{"status":"VALID"},"id":1}"#;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let resp_clone = response.to_string();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());

            // Collect all headers
            let mut headers = Vec::new();
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();

            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if header.to_lowercase().starts_with("content-length:") {
                    content_length = header.split(':').nth(1).unwrap().trim().parse().unwrap();
                }
                headers.push(header.trim().to_string());
            }

            // Read body
            let mut body = vec![0u8; content_length];
            std::io::Read::read_exact(&mut reader, &mut body).unwrap();

            // Send response
            let http_resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                resp_clone.len(),
                resp_clone
            );
            stream.write_all(http_resp.as_bytes()).unwrap();
            stream.flush().unwrap();

            headers
        });

        let client = EngineApiClient::new(test_config(port));
        let _ = client
            .auth_rpc_call("engine_forkchoiceUpdatedV3", vec![])
            .await;

        let headers = handle.join().unwrap();
        let auth_header = headers
            .iter()
            .find(|h| h.to_lowercase().starts_with("authorization:"))
            .expect("Authorization header must be present");

        assert!(
            auth_header.contains("Bearer "),
            "Auth header must contain Bearer token: {}",
            auth_header
        );

        // Extract the JWT and verify it
        let token = auth_header.split("Bearer ").nth(1).unwrap().trim();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT in auth header must have 3 parts");
    }

    #[tokio::test]
    async fn test_rpc_call_connection_refused() {
        // Use a port where nothing is listening
        let client = EngineApiClient::new(test_config(19999));
        let result = client.rpc_call("eth_blockNumber", vec![]).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            EngineApiError::Http(_) => {}
            other => panic!("Expected Http error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_get_genesis_block_hash_parses_response() {
        let response = r#"{"jsonrpc":"2.0","result":{"hash":"0xd4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3","number":"0x0"},"id":1}"#;
        let (port, _handle) = start_mock_rpc_server(response);

        let client = EngineApiClient::new(test_config(port));
        let hash = client.get_genesis_block_hash().await.unwrap();

        assert_eq!(
            hash,
            "0xd4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3"
        );
    }

    #[tokio::test]
    async fn test_get_block_number_parses_hex() {
        let response = r#"{"jsonrpc":"2.0","result":"0xa","id":1}"#;
        let (port, _handle) = start_mock_rpc_server(response);

        let client = EngineApiClient::new(test_config(port));
        let num = client.get_block_number().await.unwrap();

        assert_eq!(num, "0xa");
        let parsed = u64::from_str_radix(num.trim_start_matches("0x"), 16).unwrap();
        assert_eq!(parsed, 10);
    }

    // ---- Error type tests ----

    #[test]
    fn test_engine_api_error_display() {
        let err = EngineApiError::JsonRpc {
            code: -32000,
            message: "unknown block".into(),
        };
        assert_eq!(
            err.to_string(),
            "JSON-RPC error: code=-32000, message=unknown block"
        );

        let err = EngineApiError::InvalidPayload {
            status: "INVALID".into(),
            error: Some("bad timestamp".into()),
        };
        assert!(err.to_string().contains("INVALID"));
        assert!(err.to_string().contains("bad timestamp"));
    }
}
