use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Local;
use serde::{Deserialize, Serialize};
use solana_quic_client::{QuicConfig, QuicConnectionManager, QuicPool};
use std::time::Instant;
use std::{sync::Arc, time::Duration};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
mod tpu_client_turbo;
use base64::{engine::general_purpose, Engine as _};
use tpu_client_turbo::tpu_client_local_2::TpuClientConfig;
use tpu_client_turbo::TpuClient;

struct AppState {
    tpu_client: Arc<TpuClient<QuicPool, QuicConnectionManager, QuicConfig>>,
}

#[derive(Deserialize, Debug)]
struct TransactionRequest {
    txn: Vec<u8>,
}

#[derive(Deserialize, Debug)]
struct TransactionsRequest {
    txns: Vec<Vec<u8>>,
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();

    tracing_subscriber::fmt()
        .with_timer(tracing_subscriber::fmt::time::time())
        .with_target(false)
        .with_thread_ids(true)
        .with_level(true)
        .with_file(true)
        .with_line_number(true)
        .init();

    let rpc_url = std::env::var("RPC_URL").expect("RPC_URL must be set");
    let ws_url = std::env::var("WS_URL").expect("WS_URL must be set");

    let rpc_client = Arc::new(solana_client::nonblocking::rpc_client::RpcClient::new(
        rpc_url.to_string(),
    ));

    let config = TpuClientConfig::default();
    let tpu_client = TpuClient::new("tpu", rpc_client, &ws_url, config)
        .await
        .expect("Failed to create TPU client");

    let state = AppState {
        tpu_client: Arc::new(tpu_client),
    };

    let app = Router::new()
        .route("/", get(get_time))
        .route("/send_txn", post(handle_transaction))
        .route("/send_batch", post(handle_transactions_batched))
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(state));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3001")
        .await
        .unwrap();
    println!("Listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

const MAX_RETRIES: u32 = 10;
const INITIAL_RETRY_DELAY_MS: u64 = 100;

async fn handle_transaction(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TransactionRequest>,
) -> impl IntoResponse {
    let start_time = Instant::now();
    let mut attempt = 0;
    let mut last_error = None;
    let retry_delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS);

    info!(
        transaction_size = request.txn.len(),
        "Received transaction request"
    );

    while attempt < MAX_RETRIES {
        attempt += 1;
        let attempt_start = Instant::now();

        match state
            .tpu_client
            .try_send_wire_transaction(
                match general_purpose::STANDARD.decode(&request.txn) {
                    Ok(decoded_txn) => decoded_txn,
                    Err(e) => {
                        eprintln!("Failed to decode Base64 transaction: {}", e);
                        vec![]
                    }
                },
            )
            .await
        {
            Ok(_) => {
                let total_time = start_time.elapsed();
                info!(
                    total_time_ms = total_time.as_millis(),
                    successful_attempt = attempt,
                    "Transaction processed successfully after retries"
                );
                return Json(TransactionResponse {
                    status: "success".to_string(),
                    error: None,
                    processing_time_ms: total_time.as_millis() as u64,
                    attempts: attempt,
                })
                .into_response();
            }
            Err(err) => {
                let attempt_time = attempt_start.elapsed();
                warn!(
                    error = ?err,
                    attempt = attempt,
                    attempt_time_ms = attempt_time.as_millis(),
                    "Transaction attempt failed, retrying"
                );
                last_error = Some(err);

                if attempt < MAX_RETRIES {
                    info!(
                        retry_delay_ms = retry_delay.as_millis(),
                        "Waiting before next retry"
                    );
                }
            }
        }
    }

    // if we get here, all retries failed
    let total_time = start_time.elapsed();
    error!(
        error = ?last_error,
        total_time_ms = total_time.as_millis(),
        attempts = attempt,
        "All retry attempts failed"
    );

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(TransactionResponse {
            status: "error".to_string(),
            error: last_error.map(|e| e.to_string()),
            processing_time_ms: total_time.as_millis() as u64,
            attempts: attempt,
        }),
    )
        .into_response()
}


async fn handle_transactions_batched(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TransactionsRequest>,
) -> impl IntoResponse {
    let start_time = Instant::now();
    let mut attempt = 0;
    let mut last_error = None;
    let retry_delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS);

    info!(
        transaction_size = request.txns.len(),
        "Received transactions request"
    );

    while attempt < MAX_RETRIES {
        attempt += 1;
        let attempt_start = Instant::now();
        match state
        .tpu_client
        .try_send_wire_transaction_batch(
            request.txns.iter()
                .filter_map(|txn| {
                    match general_purpose::STANDARD.decode(txn) {
                        Ok(decoded_txn) => Some(decoded_txn),
                        Err(e) => {
                            eprintln!("Failed to decode Base64 transaction: {}", e);
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        )
        .await
        {
            Ok(_) => {
                let total_time = start_time.elapsed();
                info!(
                    total_time_ms = total_time.as_millis(),
                    successful_attempt = attempt,
                    "Transaction processed successfully after retries"
                );
                return Json(TransactionResponse {
                    status: "success".to_string(),
                    error: None,
                    processing_time_ms: total_time.as_millis() as u64,
                    attempts: attempt,
                })
                .into_response();
            }
            Err(err) => {
                let attempt_time = attempt_start.elapsed();
                warn!(
                    error = ?err,
                    attempt = attempt,
                    attempt_time_ms = attempt_time.as_millis(),
                    "Transaction attempt failed, retrying"
                );
                last_error = Some(err);

                if attempt < MAX_RETRIES {
                    info!(
                        retry_delay_ms = retry_delay.as_millis(),
                        "Waiting before next retry"
                    );
                }
            }
        }
    }

    // if we get here, all retries failed
    let total_time = start_time.elapsed();
    error!(
        error = ?last_error,
        total_time_ms = total_time.as_millis(),
        attempts = attempt,
        "All retry attempts failed"
    );

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(TransactionResponse {
            status: "error".to_string(),
            error: last_error.map(|e| e.to_string()),
            processing_time_ms: total_time.as_millis() as u64,
            attempts: attempt,
        }),
    )
        .into_response()
}


#[derive(Serialize)]
struct TransactionResponse {
    status: String,
    error: Option<String>,
    processing_time_ms: u64,
    attempts: u32,
}

async fn get_time() -> impl IntoResponse {
    let current_time = Local::now().to_rfc3339();
    Json(serde_json::json!({
        "time": current_time
    }))
}
