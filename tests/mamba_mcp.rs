use {
    anyhow::Context,
    axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    },
    rmcp::{
        ServiceExt,
        model::{
            CallToolRequestParams, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
        },
        transport::TokioChildProcess,
    },
    serde_json::{Value, json},
    std::{net::SocketAddr, path::PathBuf, process::Stdio, sync::Arc},
    tokio::io::{
        AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    },
    tokio::sync::Mutex,
};

#[derive(Clone, Default)]
struct MockApiState {
    last_transfer: Arc<Mutex<Option<Value>>>,
    last_swap: Arc<Mutex<Option<Value>>>,
}

async fn docs_handler() -> Json<Value> {
    Json(json!({
        "auth_header": "x-api-key: <MAMBA_API_KEY>",
        "base_paths": ["/mamba-api", "/mamba-api/v1"],
        "endpoints": [{"method": "GET", "path": "/health"}]
    }))
}

async fn health_handler() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "cluster": "Devnet",
        "live_sends_enabled": true,
        "signer_configured": true
    }))
}

async fn active_wallet_handler() -> Json<Value> {
    Json(json!({
        "pubkey": "active-wallet",
        "label": "main",
        "managed": true,
        "active": true,
        "selected": true,
        "balance_lamports": 1234567890_u64,
        "balance_sol": 1.23456789,
        "cluster": "Devnet",
        "timestamp_unix_ms": 123
    }))
}

async fn wallet_balance_handler(
    axum::extract::Path(wallet): axum::extract::Path<String>,
) -> Json<Value> {
    Json(json!({
        "pubkey": wallet,
        "label": Value::Null,
        "managed": false,
        "active": false,
        "selected": false,
        "balance_lamports": 420000000_u64,
        "balance_sol": 0.42,
        "cluster": "Devnet",
        "timestamp_unix_ms": 456
    }))
}

async fn metadata_batch_handler(Json(payload): Json<Value>) -> Json<Value> {
    Json(json!({
        "results": payload["mints"].as_array().cloned().unwrap_or_default().into_iter().map(|mint| {
            json!({
                "mint": mint,
                "name": "Example",
                "symbol": "EX",
                "uri": "https://example.com/token.json",
                "creator": "creator-1",
                "authority": "authority-1"
            })
        }).collect::<Vec<_>>()
    }))
}

async fn launchpad_global_configs_handler() -> Json<Value> {
    Json(json!([
        {
            "pubkey": "global-config-1",
            "curve_type": 0,
            "trade_fee_rate": 100,
            "max_share_fee_rate": 50,
            "quote_mint": "So11111111111111111111111111111111111111112"
        }
    ]))
}

async fn launchpad_platform_configs_handler() -> Json<Value> {
    Json(json!([
        {
            "pubkey": "platform-config-1",
            "platform_fee_wallet": "fee-wallet-1",
            "fee_rate": 100,
            "creator_fee_rate": 50,
            "name": "Launchpad",
            "web": "https://example.com",
            "img": "https://example.com/image.png",
            "curve_params_len": 1,
            "curve_params_global_configs": ["global-config-1"]
        }
    ]))
}

async fn launchpad_curve_params_handler(
    axum::extract::Path(platform_config): axum::extract::Path<String>,
) -> Json<Value> {
    Json(json!([
        {
            "platform_config": platform_config,
            "epoch": 1,
            "index": 0,
            "global_config": "global-config-1",
            "migrate_type": 1,
            "amm_fee_on": "quote_token",
            "supply": 1_000_000_u64,
            "total_base_sell": 500_000_u64,
            "total_quote_fund_raising": 250_000_u64,
            "vesting_total_locked_amount": 0,
            "vesting_cliff_period": 0,
            "vesting_unlock_period": 0
        }
    ]))
}

async fn transfer_execute_handler(
    State(state): State<MockApiState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    *state.last_transfer.lock().await = Some(payload.clone());
    let auth_ok = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == "test-api-key")
        .unwrap_or(false);
    (
        StatusCode::OK,
        Json(json!({
            "submitted": true,
            "success": true,
            "auth_ok": auth_ok,
            "build": payload,
            "signature": "sig-test"
        })),
    )
}

async fn swap_handler(
    State(state): State<MockApiState>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    *state.last_swap.lock().await = Some(payload.clone());
    (
        StatusCode::OK,
        Json(json!({
            "dry_run": false,
            "executed": payload.get("execute").and_then(Value::as_bool).unwrap_or(false),
            "success": true,
            "market": "pump_swap",
            "pool": "pool-1",
            "mint": payload["mint"].clone(),
            "creator": "creator-1",
            "creator_source": "market_state_fallback",
            "price": 0.00000123,
            "low_lq": false,
            "wsol_liquidity_raw": 12000000000_u64,
            "wsol_liquidity_sol": 12.0,
            "max_safe_buy_sol_raw": 10000000000_u64,
            "max_safe_buy_sol": 10.0,
            "signature": Value::Null,
            "error": Value::Null,
            "warning": Value::Null
        })),
    )
}

fn parse_tool_json(value: rmcp::model::CallToolResult) -> Value {
    if let Some(structured) = value.structured_content {
        return structured.get("data").cloned().unwrap_or(structured);
    }

    let text = value
        .content
        .first()
        .and_then(|content| content.raw.as_text())
        .map(|text| text.text.as_str())
        .expect("tool should return text or structured content");
    let parsed: Value = serde_json::from_str(text).expect("tool text should be valid JSON");
    parsed.get("data").cloned().unwrap_or(parsed)
}

fn mamba_mcp_bin() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_mamba_mcp") {
        return PathBuf::from(path);
    }

    let exe = std::env::current_exe().expect("integration test should know current executable");
    exe.parent()
        .and_then(|deps| deps.parent())
        .map(|debug_dir| debug_dir.join("mamba_mcp"))
        .filter(|path| path.exists())
        .expect("mamba_mcp binary should exist in target/debug")
}

async fn write_content_length_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(message)?;
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_content_length_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            anyhow::bail!("unexpected EOF while reading MCP headers");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let content_length = content_length.context("missing Content-Length header")?;
    let mut payload = vec![0_u8; content_length];
    reader.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

async fn read_response_with_id<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    expected_id: i64,
) -> anyhow::Result<Value> {
    loop {
        let message = read_content_length_message(reader).await?;
        if message.get("id") == Some(&json!(expected_id)) {
            return Ok(message);
        }
    }
}

fn parse_tool_result_message(message: &Value) -> Value {
    let result = message
        .get("result")
        .expect("tool call should have a JSON-RPC result");
    if let Some(structured) = result.get("structuredContent") {
        return structured
            .get("data")
            .cloned()
            .unwrap_or_else(|| structured.clone());
    }
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|entry| entry.get("text"))
        .and_then(Value::as_str)
        .expect("tool call should carry text or structured content");
    let parsed: Value = serde_json::from_str(text).expect("tool content text should be JSON");
    parsed.get("data").cloned().unwrap_or(parsed)
}

fn parse_resource_json(result: ReadResourceResult) -> Value {
    let content = result
        .contents
        .first()
        .expect("resource read should return at least one content item");
    let text = match content {
        ResourceContents::TextResourceContents { text, .. } => text,
        ResourceContents::BlobResourceContents { .. } => {
            panic!("resource read should return text content")
        }
    };
    serde_json::from_str(text).expect("resource text should be valid JSON")
}

#[tokio::test]
async fn mamba_mcp_exposes_tools_and_forwards_requests() -> anyhow::Result<()> {
    let state = MockApiState::default();
    let app = Router::new()
        .route("/mamba-api/v1/docs", get(docs_handler))
        .route("/mamba-api/v1/health", get(health_handler))
        .route("/mamba-api/v1/wallets/active", get(active_wallet_handler))
        .route(
            "/mamba-api/v1/wallets/{wallet}/balance",
            get(wallet_balance_handler),
        )
        .route(
            "/mamba-api/v1/mints/metadata-batch",
            post(metadata_batch_handler),
        )
        .route(
            "/mamba-api/v1/create/raydium_launchpad/global-configs",
            get(launchpad_global_configs_handler),
        )
        .route(
            "/mamba-api/v1/create/raydium_launchpad/platform-configs",
            get(launchpad_platform_configs_handler),
        )
        .route(
            "/mamba-api/v1/create/raydium_launchpad/platform-configs/{platform_config}/curve-params",
            get(launchpad_curve_params_handler),
        )
        .route(
            "/mamba-api/v1/wallets/transfer/execute",
            post(transfer_execute_handler),
        )
        .route("/mamba-api/v1/swap", post(swap_handler))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock API should serve");
    });

    let mut command = tokio::process::Command::new(mamba_mcp_bin());
    command.env("MAMBA_MCP_API_URL", format!("http://{addr}/mamba-api/v1"));
    command.env("MAMBA_MCP_API_KEY", "test-api-key");

    let transport = TokioChildProcess::new(command)?;
    let client = ().serve(transport).await?;

    let tools = client.list_all_tools().await?;
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"health"));
    assert!(tool_names.contains(&"get_active_wallet"));
    assert!(tool_names.contains(&"get_wallet_balance"));
    assert!(tool_names.contains(&"batch_get_token_metadata"));
    assert!(tool_names.contains(&"list_raydium_launchpad_global_configs"));
    assert!(tool_names.contains(&"list_raydium_launchpad_platform_configs"));
    assert!(tool_names.contains(&"list_raydium_launchpad_platform_curve_params"));
    assert!(tool_names.contains(&"transfer_asset"));
    assert!(tool_names.contains(&"call_mamba_api"));

    let health = client
        .call_tool(CallToolRequestParams::new("health"))
        .await?;
    let health_json = parse_tool_json(health);
    assert_eq!(health_json["status"], "ok");
    assert_eq!(health_json["cluster"], "Devnet");

    let resources = client.list_all_resources().await?;
    let resource_uris = resources
        .iter()
        .map(|resource| resource.uri.as_str())
        .collect::<Vec<_>>();
    assert!(resource_uris.contains(&"mamba://health"));
    assert!(resource_uris.contains(&"mamba://docs"));
    assert!(resource_uris.contains(&"mamba://tool-playbook"));
    assert!(resource_uris.contains(&"mamba://wallets/active"));

    let health_resource = client
        .read_resource(ReadResourceRequestParams::new("mamba://health"))
        .await?;
    let health_resource_json = parse_resource_json(health_resource);
    assert_eq!(health_resource_json["status"], "ok");
    assert_eq!(health_resource_json["cluster"], "Devnet");

    let playbook_resource = client
        .read_resource(ReadResourceRequestParams::new("mamba://tool-playbook"))
        .await?;
    let playbook_resource_json = parse_resource_json(playbook_resource);
    assert_eq!(
        playbook_resource_json["canonical_tools_by_intent"]["current_wallet_status_or_balance"][0],
        "get_active_wallet"
    );
    assert_eq!(
        playbook_resource_json["prefer_over"][0]["prefer"],
        "get_active_wallet"
    );

    let active_wallet = client
        .call_tool(CallToolRequestParams::new("get_active_wallet"))
        .await?;
    let active_wallet_json = parse_tool_json(active_wallet);
    assert_eq!(active_wallet_json["pubkey"], "active-wallet");
    assert_eq!(active_wallet_json["balance_sol"], 1.23456789);

    let active_wallet_resource = client
        .read_resource(ReadResourceRequestParams::new("mamba://wallets/active"))
        .await?;
    let active_wallet_resource_json = parse_resource_json(active_wallet_resource);
    assert_eq!(active_wallet_resource_json["pubkey"], "active-wallet");
    assert_eq!(
        active_wallet_resource_json["balance_lamports"],
        1234567890_u64
    );

    let wallet_balance = client
        .call_tool(
            CallToolRequestParams::new("get_wallet_balance").with_arguments(
                json!({
                    "wallet": "external-wallet"
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await?;
    let wallet_balance_json = parse_tool_json(wallet_balance);
    assert_eq!(wallet_balance_json["pubkey"], "external-wallet");
    assert_eq!(wallet_balance_json["balance_sol"], 0.42);

    let metadata_batch = client
        .call_tool(
            CallToolRequestParams::new("batch_get_token_metadata").with_arguments(
                json!({
                    "mints": ["mint-1", "mint-2"]
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await?;
    let metadata_batch_json = parse_tool_json(metadata_batch);
    assert_eq!(metadata_batch_json["results"].as_array().unwrap().len(), 2);
    assert_eq!(metadata_batch_json["results"][0]["name"], "Example");

    let global_configs = client
        .call_tool(CallToolRequestParams::new(
            "list_raydium_launchpad_global_configs",
        ))
        .await?;
    let global_configs_json = parse_tool_json(global_configs);
    assert_eq!(global_configs_json.as_array().unwrap().len(), 1);
    assert_eq!(global_configs_json[0]["pubkey"], "global-config-1");

    let platform_configs = client
        .call_tool(CallToolRequestParams::new(
            "list_raydium_launchpad_platform_configs",
        ))
        .await?;
    let platform_configs_json = parse_tool_json(platform_configs);
    assert_eq!(platform_configs_json.as_array().unwrap().len(), 1);
    assert_eq!(platform_configs_json[0]["pubkey"], "platform-config-1");

    let curve_params = client
        .call_tool(
            CallToolRequestParams::new("list_raydium_launchpad_platform_curve_params")
                .with_arguments(
                    json!({
                        "platform_config": "platform-config-1"
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
        )
        .await?;
    let curve_params_json = parse_tool_json(curve_params);
    assert_eq!(curve_params_json.as_array().unwrap().len(), 1);
    assert_eq!(curve_params_json[0]["platform_config"], "platform-config-1");

    let transfer = client
        .call_tool(
            CallToolRequestParams::new("transfer_asset").with_arguments(
                json!({
                    "from_wallet": "from-wallet",
                    "to_address": "to-wallet",
                    "amount": "0.01",
                    "asset_kind": "sol",
                    "execute": true,
                    "simulate": true
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await?;
    let transfer_json = parse_tool_json(transfer);
    assert_eq!(transfer_json["submitted"], true);
    assert_eq!(transfer_json["auth_ok"], true);
    assert_eq!(transfer_json["signature"], "sig-test");

    let captured = state
        .last_transfer
        .lock()
        .await
        .clone()
        .expect("mock API should capture transfer request");
    assert_eq!(captured["from_wallet"], "from-wallet");
    assert_eq!(captured["to_address"], "to-wallet");
    assert_eq!(captured["asset_kind"], "sol");
    assert_eq!(captured["execute"], Value::Null);

    let raw = client
        .call_tool(
            CallToolRequestParams::new("call_mamba_api").with_arguments(
                json!({
                    "method": "GET",
                    "path": "/docs"
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await?;
    let raw_json = parse_tool_json(raw);
    assert_eq!(raw_json["auth_header"], "x-api-key: <MAMBA_API_KEY>");

    let buy = client
        .call_tool(
            CallToolRequestParams::new("buy_token").with_arguments(
                json!({
                    "mint": "mint-1",
                    "buy_sol": 0.01,
                    "slippage_pct": 15,
                    "execute": true
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await?;
    let buy_json = parse_tool_json(buy);
    assert_eq!(buy_json["success"], true);
    let captured_swap = state
        .last_swap
        .lock()
        .await
        .clone()
        .expect("mock API should capture swap request");
    assert_eq!(captured_swap["mint"], "mint-1");
    assert_eq!(captured_swap["execute"], true);
    assert_eq!(captured_swap["skip_low_lq_pools"], true);

    client.cancel().await?;
    server_handle.abort();
    Ok(())
}

#[tokio::test]
async fn mamba_mcp_supports_standard_content_length_stdio_clients() -> anyhow::Result<()> {
    let state = MockApiState::default();
    let app = Router::new()
        .route("/mamba-api/v1/health", get(health_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("mock API should serve");
    });

    let mut command = tokio::process::Command::new(mamba_mcp_bin());
    command
        .env("MAMBA_MCP_API_URL", format!("http://{addr}/mamba-api/v1"))
        .env("MAMBA_MCP_API_KEY", "test-api-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let mut child_stdin = child.stdin.take().context("missing mamba_mcp stdin")?;
    let child_stdout = child.stdout.take().context("missing mamba_mcp stdout")?;
    let mut child_stderr = BufReader::new(child.stderr.take().context("missing mamba_mcp stderr")?);
    let mut reader = BufReader::new(child_stdout);

    write_content_length_message(
        &mut child_stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {
                    "name": "content-length-test",
                    "version": "1.0.0"
                }
            }
        }),
    )
    .await?;
    let initialize = read_response_with_id(&mut reader, 1).await?;
    assert!(initialize.get("result").is_some());

    write_content_length_message(
        &mut child_stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    )
    .await?;
    write_content_length_message(
        &mut child_stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "health",
                "arguments": {}
            }
        }),
    )
    .await?;
    let health = read_response_with_id(&mut reader, 2).await?;
    let health_json = parse_tool_result_message(&health);
    assert_eq!(health_json["status"], "ok");
    assert_eq!(health_json["cluster"], "Devnet");

    write_content_length_message(
        &mut child_stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/list",
            "params": {}
        }),
    )
    .await?;
    let resources = read_response_with_id(&mut reader, 3).await?;
    let resource_uris = resources["result"]["resources"]
        .as_array()
        .expect("resources/list should return an array")
        .iter()
        .filter_map(|entry| entry.get("uri").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(resource_uris.contains(&"mamba://health"));

    write_content_length_message(
        &mut child_stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "resources/read",
            "params": {
                "uri": "mamba://health"
            }
        }),
    )
    .await?;
    let health_resource = read_response_with_id(&mut reader, 4).await?;
    let health_resource_text = health_resource["result"]["contents"]
        .as_array()
        .and_then(|contents| contents.first())
        .and_then(|entry| entry.get("text"))
        .and_then(Value::as_str)
        .expect("resources/read should return text content");
    let health_resource_json: Value = serde_json::from_str(health_resource_text)?;
    assert_eq!(health_resource_json["status"], "ok");
    assert_eq!(health_resource_json["cluster"], "Devnet");

    drop(child_stdin);
    let status = child.wait().await?;
    let mut stderr = String::new();
    child_stderr.read_to_string(&mut stderr).await?;
    assert!(
        status.success(),
        "mamba_mcp should exit cleanly after stdin EOF: {stderr}"
    );

    server_handle.abort();
    Ok(())
}
