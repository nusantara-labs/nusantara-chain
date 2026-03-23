use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ── Health ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub status: String,
    pub slot: u64,
    pub identity: String,
    pub root_slot: u64,
    pub behind_slots: u64,
    pub peer_count: usize,
    pub epoch: u64,
    pub epoch_progress_pct: f64,
    pub consecutive_skips: u64,
    pub total_active_stake: u64,
}

// ── Account ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountResponse {
    pub address: String,
    pub lamports: u64,
    pub nusa: f64,
    pub owner: String,
    pub executable: bool,
    pub rent_epoch: u64,
    pub data_len: usize,
}

// ── Block ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockResponse {
    pub slot: u64,
    pub parent_slot: u64,
    pub parent_hash: String,
    pub block_hash: String,
    pub timestamp: i64,
    pub validator: String,
    pub transaction_count: u64,
    pub merkle_root: String,
}

// ── Block Transactions ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockTransactionEntry {
    pub signature: String,
    pub tx_index: u32,
    pub status: String,
    pub fee: u64,
    pub compute_units_consumed: u64,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockTransactionsResponse {
    pub slot: u64,
    pub transactions: Vec<BlockTransactionEntry>,
}

// ── Transaction ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransactionStatusResponse {
    pub signature: String,
    pub slot: u64,
    pub status: String,
    pub fee: u64,
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    pub compute_units_consumed: u64,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SendTransactionRequest {
    /// Base64 URL-safe no-pad encoded borsh-serialized transaction
    pub transaction: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SendTransactionResponse {
    pub signature: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SendAndConfirmRequest {
    /// Base64 URL-safe no-pad encoded borsh-serialized transaction
    pub transaction: String,
    /// Timeout in milliseconds (default: 5000, max: 30000)
    #[serde(default = "default_confirm_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_confirm_timeout_ms() -> u64 {
    5000
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SendAndConfirmResponse {
    pub signature: String,
    pub slot: u64,
    pub status: String,
    pub confirmation_time_ms: u64,
}

// ── Slot ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SlotResponse {
    pub slot: u64,
    pub latest_stored_slot: Option<u64>,
    pub latest_root: Option<u64>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockhashResponse {
    pub blockhash: String,
    pub slot: u64,
}

// ── Epoch ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EpochInfoResponse {
    pub epoch: u64,
    pub slot_index: u64,
    pub slots_in_epoch: u64,
    pub absolute_slot: u64,
    pub timestamp: i64,
    pub leader_schedule_epoch: u64,
}

// ── Leader Schedule ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaderScheduleResponse {
    pub epoch: u64,
    pub schedule: Vec<LeaderSlotEntry>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaderSlotEntry {
    pub slot: u64,
    pub leader: String,
}

// ── Validator ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidatorsResponse {
    pub total_active_stake: u64,
    pub validators: Vec<ValidatorEntry>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidatorEntry {
    pub identity: String,
    pub vote_account: String,
    pub commission: u8,
    pub active_stake: u64,
    pub last_vote: Option<u64>,
    pub root_slot: Option<u64>,
}

// ── Stake ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StakeAccountResponse {
    pub address: String,
    pub lamports: u64,
    pub state: String,
    pub staker: Option<String>,
    pub withdrawer: Option<String>,
    pub voter: Option<String>,
    pub stake: Option<u64>,
    pub activation_epoch: Option<u64>,
    pub deactivation_epoch: Option<u64>,
    pub rent_exempt_reserve: Option<u64>,
}

// ── Vote ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoteAccountResponse {
    pub address: String,
    pub lamports: u64,
    pub node_pubkey: String,
    pub authorized_voter: String,
    pub authorized_withdrawer: String,
    pub commission: u8,
    pub root_slot: Option<u64>,
    pub last_vote_slot: Option<u64>,
    pub epoch_credits: Vec<EpochCreditEntry>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EpochCreditEntry {
    pub epoch: u64,
    pub credits: u64,
    pub prev_credits: u64,
}

// ── Signatures ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignatureEntry {
    pub signature: String,
    pub slot: u64,
    pub tx_index: u32,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignaturesResponse {
    pub address: String,
    pub signatures: Vec<SignatureEntry>,
}

// ── Program ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgramResponse {
    pub address: String,
    pub executable: bool,
    pub program_data_address: String,
    pub authority: Option<String>,
    pub deploy_slot: u64,
    pub bytecode_size: usize,
    pub lamports: u64,
}

// ── Airdrop ──

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AirdropRequest {
    pub address: String,
    pub lamports: u64,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AirdropResponse {
    pub signature: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AirdropAndConfirmRequest {
    pub address: String,
    pub lamports: u64,
    /// Timeout in milliseconds (default: 5000, max: 30000)
    #[serde(default = "default_confirm_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AirdropAndConfirmResponse {
    pub signature: String,
    pub slot: u64,
    pub status: String,
    pub confirmation_time_ms: u64,
}
