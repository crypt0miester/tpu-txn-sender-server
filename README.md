# Solana TPU Transaction Sender Server

## Overview
A Rust-based Axum web server for submitting Solana transactions using a TPU (Transaction Processing Unit) client with advanced retry mechanisms.

## Architecture
- **Server**: Rust (Axum)
- **Client**: TypeScript
- **Networking**: QUIC
- **Transaction Handling**: Configurable retry strategy

## Features
- HTTP endpoints for transaction submission
- Single and batch transaction support
- Comprehensive error handling
- Detailed transaction processing logs
- Configurable retry mechanism

## Endpoints
- `GET /`: Server time
- `POST /send_txn`: Single transaction submission
- `POST /send_batch`: Batch transaction submission

## Quick Start

### Server Setup
1. Install Rust and Cargo
2. Set environment variables:
   - `RPC_URL`: Solana RPC endpoint
   - `WS_URL`: Solana WebSocket URL
3. Run `cargo run`

### TypeScript Client Example
```typescript
async function submitTransaction(transactionBytes: Buffer) {
  const response = await fetch('http://localhost:3001/send_txn', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ txn: Array.from(transactionBytes) })
  });
  return response.json();
}
```

## Transaction Retry Strategy
- Maximum retries: 10
- Initial retry delay: 100ms
- Exponential backoff recommended

## Logging
Uses `tracing` for detailed logs including:
- Timestamps
- Thread IDs
- Error context
- Performance metrics

## Performance Considerations
- Utilizes QUIC for efficient networking
- Configurable transaction processing
- Supports both single and batch transactions