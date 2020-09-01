import bs58 from "bs58";
import {
  SystemProgram,
  StakeProgram,
  VOTE_PROGRAM_ID,
  BpfLoader,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_RENT_PUBKEY,
  SYSVAR_REWARDS_PUBKEY,
  SYSVAR_STAKE_HISTORY_PUBKEY,
  ParsedTransaction,
  TransactionInstruction,
  Transaction,
} from "@solana/web3.js";
import { TokenRegistry } from "tokenRegistry";
import { Cluster } from "providers/cluster";

export type ProgramName =
  | "Budget Program"
  | "Config Program"
  | "Exchange Program"
  | "Stake Program"
  | "Storage Program"
  | "System Program"
  | "Vest Program"
  | "Vote Program"
  | "SPL Token Program";

export const EXTERNAL_PROGRAMS: { [key: string]: string } = {
  Serum: "4ckmDgGdxQoPDLUkDT3vHgSAkzA3QRdNq5ywwY4sUSJn",
};

export const PROGRAM_IDS: { [key: string]: ProgramName } = {
  Budget1111111111111111111111111111111111111: "Budget Program",
  Config1111111111111111111111111111111111111: "Config Program",
  Exchange11111111111111111111111111111111111: "Exchange Program",
  [StakeProgram.programId.toBase58()]: "Stake Program",
  Storage111111111111111111111111111111111111: "Storage Program",
  [SystemProgram.programId.toBase58()]: "System Program",
  Vest111111111111111111111111111111111111111: "Vest Program",
  [VOTE_PROGRAM_ID.toBase58()]: "Vote Program",
  TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA: "SPL Token Program",
};

const LOADER_IDS = {
  MoveLdr111111111111111111111111111111111111: "Move Loader",
  NativeLoader1111111111111111111111111111111: "Native Loader",
  [BpfLoader.programId(1).toBase58()]: "BPF Loader",
  [BpfLoader.programId(2).toBase58()]: "BPF Loader 2",
};

const SYSVAR_ID: { [key: string]: string } = {
  Sysvar1111111111111111111111111111111111111: "SYSVAR",
};

export const SYSVAR_IDS = {
  [SYSVAR_CLOCK_PUBKEY.toBase58()]: "SYSVAR_CLOCK",
  SysvarEpochSchedu1e111111111111111111111111: "SYSVAR_EPOCH_SCHEDULE",
  SysvarFees111111111111111111111111111111111: "SYSVAR_FEES",
  SysvarRecentB1ockHashes11111111111111111111: "SYSVAR_RECENT_BLOCKHASHES",
  [SYSVAR_RENT_PUBKEY.toBase58()]: "SYSVAR_RENT",
  [SYSVAR_REWARDS_PUBKEY.toBase58()]: "SYSVAR_REWARDS",
  SysvarS1otHashes111111111111111111111111111: "SYSVAR_SLOT_HASHES",
  SysvarS1otHistory11111111111111111111111111: "SYSVAR_SLOT_HISTORY",
  [SYSVAR_STAKE_HISTORY_PUBKEY.toBase58()]: "SYSVAR_STAKE_HISTORY",
};

export function addressLabel(
  address: string,
  cluster: Cluster
): string | undefined {
  return (
    PROGRAM_IDS[address] ||
    LOADER_IDS[address] ||
    SYSVAR_IDS[address] ||
    SYSVAR_ID[address] ||
    TokenRegistry.get(address, cluster)?.name
  );
}

export function displayAddress(address: string, cluster: Cluster): string {
  return addressLabel(address, cluster) || address;
}

export function intoTransactionInstruction(
  tx: ParsedTransaction,
  index: number
): TransactionInstruction | undefined {
  const message = tx.message;
  const instruction = message.instructions[index];
  if ("parsed" in instruction) return;

  const keys = [];
  for (const account of instruction.accounts) {
    const accountKey = message.accountKeys.find(({ pubkey }) =>
      pubkey.equals(account)
    );
    if (!accountKey) return;
    keys.push({
      pubkey: accountKey.pubkey,
      isSigner: accountKey.signer,
      isWritable: accountKey.writable,
    });
  }

  return new TransactionInstruction({
    data: bs58.decode(instruction.data),
    keys: keys,
    programId: instruction.programId,
  });
}

export function intoParsedTransaction(tx: Transaction): ParsedTransaction {
  const message = tx.compileMessage();
  return {
    signatures: tx.signatures.map((value) =>
      bs58.encode(value.signature as any)
    ),
    message: {
      accountKeys: message.accountKeys.map((key, index) => ({
        pubkey: key,
        signer: tx.signatures.some(({ publicKey }) => publicKey.equals(key)),
        writable: message.isAccountWritable(index),
      })),
      instructions: message.instructions.map((ix) => ({
        programId: message.accountKeys[ix.programIdIndex],
        accounts: ix.accounts.map((index) => message.accountKeys[index]),
        data: ix.data,
      })),
      recentBlockhash: message.recentBlockhash,
    },
  };
}

export function isSerumInstruction(instruction: TransactionInstruction) {
  return instruction.programId.toBase58() === EXTERNAL_PROGRAMS["Serum"];
}

type ParsedSerumInstruction = {
  code: number;
  operation: string;
};

const SERUM_CODE_LOOKUP: { [key: number]: string } = {
  0: "Initialize Market",
  1: "New Order",
  2: "Match Order",
  3: "Consume Events",
  4: "Cancel Order",
  5: "Settle Funds",
  6: "Cancel Order By Client Id",
  7: "Disable Market",
  8: "Sweep Fees",
};

export function parseSerumInstruction(
  instruction: TransactionInstruction
): ParsedSerumInstruction {
  const code = instruction.data.slice(1, 5).readUInt32LE(0);

  let operation = "Unknown";

  if (code in SERUM_CODE_LOOKUP) {
    operation = SERUM_CODE_LOOKUP[code];
  }

  return {
    code,
    operation,
  };
}
