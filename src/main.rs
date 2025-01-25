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
use tpu_client_turbo::tpu_client_local_2::TpuClientConfig;
use tpu_client_turbo::TpuClient;

struct AppState {
    tpu_client: Arc<TpuClient<QuicPool, QuicConnectionManager, QuicConfig>>,
}

#[derive(Deserialize, Debug)]
struct TransactionRequest {
    txn: Vec<u8>,
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
        .send_transaction_to_upcoming_leaders(
            // Decode from Base64
            match base64::decode(&request.txn) {
                Ok(decoded_txn) => {
                    // Encode to Base58 (or bincode if needed)
                    // let base58_txn = bs58::encode(&decoded_txn).into_string();
                    // // If bincode is needed instead:
                    // let bincode_txn = bincode::serialize(&base58_txn).unwrap();
                    decoded_txn
                }
                Err(e) => {
                    eprintln!("Failed to decode Base64 transaction: {}", e);
                    vec![]
                }
            }
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

    // If we get here, all retries failed
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
