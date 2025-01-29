import {
  address,
  appendTransactionMessageInstructions,
  Base64EncodedWireTransaction,
  createKeyPairSignerFromBytes,
  createSolanaRpc,
  createSolanaRpcSubscriptions,
  createTransactionMessage,
  FullySignedTransaction,
  getBase64EncodedWireTransaction,
  getSignatureFromTransaction,
  pipe,
  Rpc,
  RpcSubscriptions,
  sendTransactionWithoutConfirmingFactory,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  SolanaRpcApi,
  SolanaRpcSubscriptionsApi,
  TransactionWithBlockhashLifetime,
} from "@solana/web3.js";
import "dotenv/config";
import { getTransferSolInstruction } from "@solana-program/system";
import {
  createBlockHeightExceedencePromiseFactory,
  createRecentSignatureConfirmationPromiseFactory,
  waitForRecentTransactionConfirmation,
} from "@solana/transaction-confirmation";
import {
  getSetComputeUnitLimitInstruction,
  getSetComputeUnitPriceInstruction,
  getSetLoadedAccountsDataSizeLimitInstruction,
} from "@solana-program/compute-budget";
import { isArray } from "util";
import pino from 'pino';
import pretty from 'pino-pretty';

const logger = pino(
  pretty({
    colorize: true,
    translateTime: 'yyyy-mm-dd HH:MM:ss.l',
    ignore: 'pid,hostname',
    customPrettifiers: {
      time: (timestamp) => `[${timestamp}]`
    }
  })
);

type Client = {
  rpc: Rpc<SolanaRpcApi>;
  rpcSubscriptions: RpcSubscriptions<SolanaRpcSubscriptionsApi>;
};

export const createDefaultSolanaClient = (): Client => {
  const rpc = createSolanaRpc(process.env.RPC_URL || "");
  const rpcSubscriptions = createSolanaRpcSubscriptions(
    process.env.WS_URL || "",
  );
  return { rpc, rpcSubscriptions };
};

const client = createDefaultSolanaClient();

const keypairFile = Bun.file(process.env.KEYPAIR_FILEPATH || "");
const keypairJson = await keypairFile.json();
const feePayer = await createKeyPairSignerFromBytes(
  Uint8Array.from(keypairJson),
);
logger.info("payer: " + feePayer.address.slice(0,10));
const destination = address(
  process.env.DESTINATION_ADDRESS ||
  "2EGGxj2qbNAJNgLCPKca8sxZYetyTjnoRspTPjzN2D67",
);

// create and sign a transaction
async function createAndSignTransaction(index: number) {
  const latestBlockhash = await client.rpc.getLatestBlockhash().send();

  const setComputeUnit = getSetComputeUnitLimitInstruction({ units: 1_000 });
  const setComputeUnitPrice = getSetComputeUnitPriceInstruction({
    microLamports: 10_000,
  });
  const setAccountDataLimitSize = getSetLoadedAccountsDataSizeLimitInstruction({
    accountDataSizeLimit: 1_000,
  });

  const transactionMessage = pipe(
    createTransactionMessage({ version: 0 }),
    (tx) => setTransactionMessageFeePayer(feePayer.address, tx),
    (tx) =>
      setTransactionMessageLifetimeUsingBlockhash(latestBlockhash.value, tx),
    (tx) =>
      appendTransactionMessageInstructions(
        [
          setAccountDataLimitSize,
          setComputeUnit,
          setComputeUnitPrice,
          getTransferSolInstruction({
            source: feePayer,
            destination,
            // adds uniqueness
            amount: 1_000 + index, // amount in lamports
          }),
        ],
        tx,
      ),
  );

  return signTransactionMessageWithSigners(transactionMessage);
}

// submit transaction
async function submitTransaction(
  transactionBytes:
    | Base64EncodedWireTransaction
    | Base64EncodedWireTransaction[],
  batched: boolean = false,
) {
  try {
    let url = batched
      ? "http://localhost:3001/send_batch"
      : "http://localhost:3001/send_txn";

    const body = Array.isArray(transactionBytes)
      ? JSON.stringify({
        txns: transactionBytes.map((txnBytes) => Array.from(Buffer.from(txnBytes))),
      })
      : JSON.stringify({
        txn: Array.from(Buffer.from(transactionBytes)),
      });

    const response = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body,
    });
    const result = await response.json();

    if (result.status === "error") {
      throw Error(result.error);
    }
    logger.info({ result }, "txn submit result");
    return result;
  } catch (error) {
    logger.error({ error }, "transaction submit error");
    throw error;
  }
}

// mock send multiple transactions
async function sendMultipleTransactions(count: number) {
  const transactions: Readonly<
    FullySignedTransaction & TransactionWithBlockhashLifetime
  >[] = [];

  for (let i = 0; i < count; i++) {
    const signedTransaction = await createAndSignTransaction(i);
    transactions.push(signedTransaction);
  }

  // submit txns sequentially
  const submissionPromises = transactions.map(async (signedTransaction) => {
    await submitTransaction(getBase64EncodedWireTransaction(signedTransaction));
    const signature = getSignatureFromTransaction(signedTransaction);
    logger.info(`signature: https://solscan.io/tx/${signature}`);

    // confirm txn
    const getBlockHeightExceedencePromise =
      createBlockHeightExceedencePromiseFactory(client);
    const getRecentSignatureConfirmationPromise =
      createRecentSignatureConfirmationPromiseFactory(client);

    await waitForRecentTransactionConfirmation({
      commitment: "processed",
      getBlockHeightExceedencePromise,
      getRecentSignatureConfirmationPromise,
      transaction: signedTransaction,
    });
    logger.info("txn confirmed");
  });

  // submit txns batched
  await Promise.all(submissionPromises);
}

// mock send multiple transactions
async function sendMultipleTransactionsBatched(count: number) {
  const transactions: Readonly<
    FullySignedTransaction & TransactionWithBlockhashLifetime
  >[] = [];

  for (let i = 0; i < count; i++) {
    const signedTransaction = await createAndSignTransaction(i);
    transactions.push(signedTransaction);
  }

  // submit batch
  await submitTransaction(
    transactions.map((txn) => getBase64EncodedWireTransaction(txn)),
    true,
  );

  const submissionPromises = transactions.map(async (signedTransaction, index) => {
    const signature = getSignatureFromTransaction(signedTransaction);
    logger.info(`signature: https://solscan.io/tx/${signature}`);

    // confirm txn
    const getBlockHeightExceedencePromise =
      createBlockHeightExceedencePromiseFactory(client);
    const getRecentSignatureConfirmationPromise =
      createRecentSignatureConfirmationPromiseFactory(client);

    await waitForRecentTransactionConfirmation({
      commitment: "confirmed",
      getBlockHeightExceedencePromise,
      getRecentSignatureConfirmationPromise,
      transaction: signedTransaction,
    });
    logger.info("txn confirmed: " + index);
  });

  // submit txns batched
  await Promise.all(submissionPromises);
}

async function main() {
  const numberOfTransactions = 1; // no. of transactions to send

  // sends them one by one in a loop
  // await sendMultipleTransactions(numberOfTransactions);

  // sends txns batched in one request.
  await sendMultipleTransactionsBatched(numberOfTransactions);
  logger.info("txns successfully sent");
}

main().catch((error) => {
  logger.error({ error }, "error in main function");
  process.exit(1);
});