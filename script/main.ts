import {
  address,
  appendTransactionMessageInstructions,
  Base64EncodedBytes,
  Base64EncodedWireTransaction,
  createKeyPairSignerFromBytes,
  createSolanaRpc,
  createSolanaRpcSubscriptions,
  createTransactionMessage,
  FullySignedTransaction,
  getBase64EncodedWireTransaction,
  getCompiledTransactionMessageEncoder,
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
  TransactionMessageBytes,
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
const destination = address(process.env.DESTINATION_ADDRESS || "2EGGxj2qbNAJNgLCPKca8sxZYetyTjnoRspTPjzN2D67");

// Function to create and sign a transaction
async function createAndSignTransaction() {
  const latestBlockhash = await client.rpc.getLatestBlockhash().send();

  const setComputeUnit = getSetComputeUnitLimitInstruction({ units: 1_000 });
  const setComputeUnitPrice = getSetComputeUnitPriceInstruction({
    microLamports: 100_000,
  });
  const setAccountDataLimitSize = getSetLoadedAccountsDataSizeLimitInstruction({
    accountDataSizeLimit: 5_000,
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
            amount: 1_000, // Amount in lamports
          }),
        ],
        tx,
      ),
  );

  return signTransactionMessageWithSigners(transactionMessage);
}

// Function to submit a transaction
async function submitTransaction(transactionBytes: Base64EncodedWireTransaction) {
  try {
    const response = await fetch("http://localhost:3001/send_txn", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        txn: Array.from(Buffer.from(transactionBytes)),
      }),
    });
    console.log(response)
    const result = await response.json();

    if (result.status === "error") {
      throw Error(result.error)
    }
    console.log("Transaction Result:", result);
    return result;
  } catch (error) {
    console.error("Transaction Submission Error:", error);
    throw error;
  }
}

// Function to send multiple transactions
async function sendMultipleTransactions(count: number) {
  const transactions: Readonly<
    FullySignedTransaction & TransactionWithBlockhashLifetime
  >[] = [];

  // Create and sign multiple transactions
  for (let i = 0; i < count; i++) {
    const signedTransaction = await createAndSignTransaction();
    transactions.push(signedTransaction);
  }

  // Submit all transactions concurrently
  const submissionPromises = transactions.map(async (signedTransaction) => {
    await submitTransaction(getBase64EncodedWireTransaction(signedTransaction));
    // await sendTransactionWithoutConfirmingFactory({ rpc: client.rpc })(
    //   signedTransaction,
    //   {
    //     commitment: "confirmed",
    //   },
    // );
    const signature = getSignatureFromTransaction(signedTransaction);
    console.log("Transaction Signature:", signature);
    console.log("https://solscan.io/tx/" + signature);

    // Wait for confirmation
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
  });

  // Wait for all transactions to complete
  await Promise.all(submissionPromises);
}

// Main function
async function main() {
  const numberOfTransactions = 10; // Number of transactions to send
  await sendMultipleTransactions(numberOfTransactions);
  console.log("All transactions submitted successfully!");
}

// Run the script
main().catch((error) => {
  console.error("Error in main function:", error);
  process.exit(1);
});
