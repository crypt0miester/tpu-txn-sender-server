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
console.log("feePayer", feePayer.address);
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
    let body = "";
    if (isArray(transactionBytes)) {
      body = JSON.stringify({
        txns: transactionBytes.map((txnBytes) =>
          Array.from(Buffer.from(txnBytes)),
        ),
      });
    } else {
      body = JSON.stringify({
        txn: Array.from(Buffer.from(transactionBytes)),
      });
    }
    const response = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body,
    });
    // console.log(response)
    const result = await response.json();

    if (result.status === "error") {
      throw Error(result.error);
    }
    console.log("submit result:", result);
    return result;
  } catch (error) {
    console.error("txn submit error:", error);
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

  // submit all txns concurrently
  const submissionPromises = transactions.map(async (signedTransaction) => {
    await submitTransaction(getBase64EncodedWireTransaction(signedTransaction));
    // await sendTransactionWithoutConfirmingFactory({ rpc: client.rpc })(
    //   signedTransaction,
    //   {
    //     commitment: "confirmed",
    //   },
    // );
    const signature = getSignatureFromTransaction(signedTransaction);
    console.log("signature: https://solscan.io/tx/" + signature);

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
    console.log("txn confirmed");
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

  const submissionPromises = transactions.map(async (signedTransaction) => {
    const signature = getSignatureFromTransaction(signedTransaction);
    console.log("signature: https://solscan.io/tx/" + signature);

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
    console.log("txn confirmed");
  });

  // submit txns batched
  await Promise.all(submissionPromises);
}

async function main() {
  const numberOfTransactions = 4; // no. of transactions to send

  // sends them one by one in a loop
  // await sendMultipleTransactions(numberOfTransactions);

  // sends txns batched in one request.
  await sendMultipleTransactionsBatched(numberOfTransactions);
  console.log("txns submitted!");
}

main().catch((error) => {
  console.error("Error in main function:", error);
  process.exit(1);
});
