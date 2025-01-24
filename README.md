# Solana TPU Transaction Sender Server

## Overview
A Rust-based Axum web server for submitting Solana transactions using a TPU (Transaction Processing Unit) client with retry logic.

## Features
- Send Solana transactions via HTTP POST
- Configurable retry mechanism
- Detailed transaction processing logging
- Supports QUIC networking

## Dependencies
- Axum
- Solana Client
- Tokio
- Tracing

## Server Endpoints
- `GET /`: Returns current server time
- `POST /send_txn`: Submit Solana transaction

## Configuration
Modify `rpc_url` and `ws_url` in `main()` with your Solana cluster endpoints.

## TypeScript Test Client

```typescript
async function submitTransaction(transactionBytes: Buffer) {
  try {
    const response = await fetch('http://localhost:3001/send_txn', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        txn: Array.from(transactionBytes)  // Convert Buffer to array
      })
    });

    const result = await response.json();
    console.log('Transaction Result:', result);
    return result;
  } catch (error) {
    console.error('Transaction Submission Error:', error);
    throw error;
  }
}

// Example usage
const testTransaction = Buffer.from('your_serialized_transaction_bytes');
submitTransaction(testTransaction);
```

## Running the Server
1. Ensure Rust and Cargo are installed
2. Run `cargo run`

## Transaction Retry Strategy
- Maximum 10 retries
- Initial retry delay of 100ms
- Exponential backoff recommended for production

## Logging
Uses `tracing` for comprehensive logging with:
- Timestamps
- Thread IDs
- File and line number information