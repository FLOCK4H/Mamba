use {
    crate::core::create::{
        SOLANA_MAX_TX_WIRE_BYTES, compile_unsigned_v0_transaction, derive_associated_token_address,
        encode_transaction_base64,
    },
    anyhow::{Context, bail},
    chrono::Utc,
    serde::{Deserialize, Serialize},
    serde_json::Value,
    solana_account_decoder_client_types::UiAccountData,
    solana_client::{nonblocking::rpc_client::RpcClient, rpc_request::TokenAccountsFilter},
    solana_commitment_config::CommitmentConfig,
    solana_keypair::Keypair,
    solana_program::{instruction::Instruction, program_pack::Pack, pubkey::Pubkey},
    solana_rpc_client_types::response::RpcKeyedAccount,
    solana_signer::Signer,
    solana_system_interface::instruction as system_instruction,
    spl_associated_token_account::instruction::create_associated_token_account_idempotent,
    spl_token::state::Mint as SplMint,
    spl_token_2022::{extension::StateWithExtensions, state::Mint as SplToken2022Mint},
    std::{
        collections::{BTreeMap, BTreeSet, HashMap, HashSet},
        fs,
        path::{Path, PathBuf},
        str::FromStr,
        sync::Arc,
    },
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub const MAMBA_WALLET_STORE_PATH_ENV: &str = "MAMBA_WALLET_STORE_PATH";

const DEFAULT_CONFIG_SUBDIR: &str = "mamba";
const DEFAULT_STORE_FILENAME: &str = "wallets.json";
const STORE_VERSION: u32 = 1;
const MAX_WALLET_CLEAN_BATCH_TX_BYTES: usize = SOLANA_MAX_TX_WIRE_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedWalletExportFormat {
    Json,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagedWalletStore {
    pub version: u32,
    pub active_wallet_pubkey: Option<String>,
    pub selected_wallet_pubkeys: Vec<String>,
    pub wallets: Vec<ManagedWalletRecord>,
}

impl Default for ManagedWalletStore {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            active_wallet_pubkey: None,
            selected_wallet_pubkeys: Vec::new(),
            wallets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedWalletRecord {
    pub pubkey: String,
    pub label: String,
    pub secret_key_base58: String,
    pub created_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedWalletExportRecord {
    pub pubkey: String,
    pub label: String,
    pub secret_key_base58: String,
    pub created_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedWalletSummary {
    pub pubkey: String,
    pub label: String,
    pub created_at_utc: String,
    pub active: bool,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct ManagedWalletRuntime {
    store: ManagedWalletStore,
    signers: HashMap<Pubkey, Arc<Keypair>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletTransferAssetKind {
    Sol,
    Token,
}

#[derive(Debug, Clone)]
pub struct WalletTransferBuildParams {
    pub from: Pubkey,
    pub to: Pubkey,
    pub amount: String,
    pub asset_kind: WalletTransferAssetKind,
    pub mint: Option<Pubkey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletTransferBuildResponse {
    pub transaction: String,
    pub required_signers: Vec<String>,
    pub derived_addresses: BTreeMap<String, String>,
    pub kind: String,
    pub amount_input: String,
    pub amount_raw: String,
    pub decimals: u8,
    pub mint: Option<String>,
    pub token_program: Option<String>,
    pub simulation: Option<WalletTransferSimulation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletTransferSimulation {
    pub ok: bool,
    pub err: Option<String>,
    pub units_consumed: Option<u64>,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletCleanActionKind {
    UnwrapWsol,
    BurnAndClose,
    CloseEmpty,
    Skip,
}

impl WalletCleanActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::UnwrapWsol => "unwrap_wsol",
            Self::BurnAndClose => "burn_and_close",
            Self::CloseEmpty => "close_empty",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletCleanEntry {
    pub token_account: String,
    pub mint: String,
    pub token_program: String,
    pub owner: String,
    pub action: WalletCleanActionKind,
    pub amount_raw: u64,
    pub amount_ui: Option<f64>,
    pub decimals: u8,
    pub reclaim_lamports: u64,
    pub reclaim_sol: f64,
    pub burn_required: bool,
    pub is_associated: bool,
    pub is_native_wsol: bool,
    pub state: Option<String>,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletCleanPreview {
    pub owner: String,
    pub total_token_accounts: usize,
    pub cleanable_accounts: usize,
    pub burn_accounts: usize,
    pub close_only_accounts: usize,
    pub unwrap_accounts: usize,
    pub blocked_accounts: usize,
    pub total_reclaim_lamports: u64,
    pub total_reclaim_sol: f64,
    pub entries: Vec<WalletCleanEntry>,
}

#[derive(Debug, Clone)]
pub struct WalletCleanBuildParams {
    pub owner: Pubkey,
    pub token_accounts: Vec<Pubkey>,
    pub burn_nonzero: bool,
    pub close_empty: bool,
    pub close_wsol: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletCleanSimulation {
    pub ok: bool,
    pub err: Option<String>,
    pub units_consumed: Option<u64>,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletCleanBatch {
    pub batch_index: usize,
    pub transaction: String,
    pub required_signers: Vec<String>,
    pub action_count: usize,
    pub token_account_count: usize,
    pub reclaim_lamports: u64,
    pub reclaim_sol: f64,
    pub actions: Vec<WalletCleanEntry>,
    pub simulation: Option<WalletCleanSimulation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletCleanBuild {
    pub owner: String,
    pub burn_nonzero: bool,
    pub close_empty: bool,
    pub close_wsol: bool,
    pub selected_account_count: usize,
    pub selected_reclaim_lamports: u64,
    pub selected_reclaim_sol: f64,
    pub preview: WalletCleanPreview,
    pub batches: Vec<WalletCleanBatch>,
}

impl ManagedWalletStore {
    pub fn load(path: &Path) -> anyhow::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }

        let raw = fs::read_to_string(path)
            .with_context(|| format!("read wallet store {}", path.display()))?;
        let mut parsed: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parse wallet store {}", path.display()))?;
        parsed.normalize();
        Ok(Some(parsed))
    }

    pub fn load_or_default(path: &Path) -> anyhow::Result<Self> {
        Ok(Self::load(path)?.unwrap_or_default())
    }

    pub fn save(&mut self, path: &Path) -> anyhow::Result<()> {
        self.version = STORE_VERSION;
        self.normalize();
        let json = serde_json::to_string_pretty(self).context("serialize wallet store")?;
        write_secure_text_file(path, &json)
    }

    pub fn export_wallets(
        &self,
        selectors: &[String],
    ) -> anyhow::Result<Vec<ManagedWalletExportRecord>> {
        if selectors.is_empty() {
            return Ok(self
                .wallets
                .iter()
                .map(ManagedWalletExportRecord::from)
                .collect());
        }

        let mut exports = Vec::with_capacity(selectors.len());
        let mut seen_pubkeys = BTreeSet::new();
        for selector in selectors {
            let wallet = self.resolve_wallet_selector(selector)?;
            if seen_pubkeys.insert(wallet.pubkey.clone()) {
                exports.push(ManagedWalletExportRecord::from(wallet));
            }
        }
        Ok(exports)
    }

    pub fn create_generated_wallet(
        &mut self,
        label: Option<&str>,
    ) -> anyhow::Result<ManagedWalletSummary> {
        let signer = Keypair::new();
        let pubkey = signer.pubkey().to_string();
        let label = label
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.next_default_label());
        let created_at_utc = Utc::now().to_rfc3339();

        self.wallets.push(ManagedWalletRecord {
            pubkey: pubkey.clone(),
            label,
            secret_key_base58: bs58::encode(signer.to_bytes()).into_string(),
            created_at_utc,
        });
        self.active_wallet_pubkey = Some(pubkey.clone());
        self.selected_wallet_pubkeys = vec![pubkey.clone()];
        self.normalize();

        self.public_wallets()
            .into_iter()
            .find(|wallet| wallet.pubkey == pubkey)
            .context("new wallet missing from store after insert")
    }

    pub fn ensure_stored_signer(
        &mut self,
        signer: &Keypair,
        preferred_label: &str,
    ) -> anyhow::Result<(ManagedWalletSummary, bool)> {
        let pubkey = signer.pubkey().to_string();
        let mut changed = false;

        if !self.wallets.iter().any(|wallet| wallet.pubkey == pubkey) {
            self.wallets.push(ManagedWalletRecord {
                pubkey: pubkey.clone(),
                label: self.unique_label(preferred_label, &pubkey),
                secret_key_base58: bs58::encode(signer.to_bytes()).into_string(),
                created_at_utc: Utc::now().to_rfc3339(),
            });
            changed = true;
        }

        if self.selected_wallet_pubkeys.is_empty() {
            self.selected_wallet_pubkeys.push(pubkey.clone());
            changed = true;
        }

        if self.active_wallet_pubkey.is_none() {
            self.active_wallet_pubkey = Some(pubkey.clone());
            changed = true;
        }

        self.normalize();
        let summary = self
            .public_wallets()
            .into_iter()
            .find(|wallet| wallet.pubkey == pubkey)
            .context("stored signer missing from wallet summaries")?;
        Ok((summary, changed))
    }

    pub fn set_active_wallet(&mut self, pubkey: &Pubkey) -> anyhow::Result<()> {
        let raw = pubkey.to_string();
        if !self.wallets.iter().any(|wallet| wallet.pubkey == raw) {
            bail!("wallet not found: {pubkey}");
        }
        self.active_wallet_pubkey = Some(raw.clone());
        if !self
            .selected_wallet_pubkeys
            .iter()
            .any(|value| value == &raw)
        {
            self.selected_wallet_pubkeys.push(raw);
        }
        self.normalize();
        Ok(())
    }

    pub fn set_selected_wallets(
        &mut self,
        pubkeys: &[Pubkey],
        active: Option<Pubkey>,
    ) -> anyhow::Result<()> {
        let known = self
            .wallets
            .iter()
            .map(|wallet| wallet.pubkey.clone())
            .collect::<BTreeSet<_>>();

        let mut selected = Vec::new();
        for pubkey in pubkeys {
            let raw = pubkey.to_string();
            if !known.contains(&raw) {
                bail!("wallet not found: {pubkey}");
            }
            if !selected.iter().any(|value| value == &raw) {
                selected.push(raw);
            }
        }

        if let Some(active_wallet) = active {
            let raw = active_wallet.to_string();
            if !known.contains(&raw) {
                bail!("wallet not found: {active_wallet}");
            }
            self.active_wallet_pubkey = Some(raw.clone());
        } else {
            self.active_wallet_pubkey = self
                .active_wallet_pubkey
                .as_ref()
                .filter(|value| known.contains(*value))
                .cloned();
        }

        self.selected_wallet_pubkeys = selected;
        self.normalize();
        Ok(())
    }

    pub fn public_wallets(&self) -> Vec<ManagedWalletSummary> {
        let selected = self
            .selected_wallet_pubkeys
            .iter()
            .map(|value| value.as_str())
            .collect::<BTreeSet<_>>();

        self.wallets
            .iter()
            .map(|wallet| ManagedWalletSummary {
                pubkey: wallet.pubkey.clone(),
                label: wallet.label.clone(),
                created_at_utc: wallet.created_at_utc.clone(),
                active: selected.contains(wallet.pubkey.as_str()),
                selected: selected.contains(wallet.pubkey.as_str()),
            })
            .collect()
    }

    fn next_default_label(&self) -> String {
        format!("wallet-{:02}", self.wallets.len() + 1)
    }

    fn unique_label(&self, preferred_label: &str, pubkey: &str) -> String {
        let preferred_label = preferred_label.trim();
        let base = if preferred_label.is_empty() {
            "wallet"
        } else {
            preferred_label
        };
        if !self.wallets.iter().any(|wallet| wallet.label == base) {
            return base.to_string();
        }

        let short = pubkey.chars().take(6).collect::<String>();
        let derived = format!("{base}-{short}");
        if !self.wallets.iter().any(|wallet| wallet.label == derived) {
            return derived;
        }

        for idx in 2.. {
            let candidate = format!("{base}-{short}-{idx:02}");
            if !self.wallets.iter().any(|wallet| wallet.label == candidate) {
                return candidate;
            }
        }

        unreachable!("label search must eventually find an unused suffix")
    }

    fn resolve_wallet_selector(&self, selector: &str) -> anyhow::Result<&ManagedWalletRecord> {
        let selector = selector.trim();
        if selector.is_empty() {
            bail!("wallet selector cannot be empty");
        }

        if let Some(wallet) = self.wallets.iter().find(|wallet| wallet.pubkey == selector) {
            return Ok(wallet);
        }

        let mut matches = self
            .wallets
            .iter()
            .filter(|wallet| wallet.label == selector);
        let Some(first) = matches.next() else {
            bail!("wallet not found for selector: {selector}");
        };
        if matches.next().is_some() {
            bail!("wallet label is ambiguous: {selector} (use the wallet pubkey instead)");
        }
        Ok(first)
    }

    fn normalize(&mut self) {
        self.version = STORE_VERSION;

        let mut seen = BTreeSet::new();
        self.wallets.retain(|wallet| {
            let pubkey = wallet.pubkey.trim();
            !pubkey.is_empty() && seen.insert(pubkey.to_string())
        });

        let known = self
            .wallets
            .iter()
            .map(|wallet| wallet.pubkey.clone())
            .collect::<BTreeSet<_>>();

        self.selected_wallet_pubkeys = self
            .selected_wallet_pubkeys
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && known.contains(value))
            .collect::<Vec<_>>();
        self.selected_wallet_pubkeys.dedup();

        let active_valid = self
            .active_wallet_pubkey
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty() && known.contains(*value))
            .map(str::to_string);

        self.active_wallet_pubkey = active_valid;
    }
}

impl From<&ManagedWalletRecord> for ManagedWalletExportRecord {
    fn from(value: &ManagedWalletRecord) -> Self {
        Self {
            pubkey: value.pubkey.clone(),
            label: value.label.clone(),
            secret_key_base58: value.secret_key_base58.clone(),
            created_at_utc: value.created_at_utc.clone(),
        }
    }
}

impl ManagedWalletRuntime {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let store = ManagedWalletStore::load_or_default(path)?;
        let mut signers = HashMap::new();

        for wallet in store.wallets.iter() {
            let signer = parse_stored_signer(wallet)?;
            signers.insert(signer.pubkey(), Arc::new(signer));
        }

        Ok(Self { store, signers })
    }

    pub fn active_pubkey(&self) -> Option<Pubkey> {
        let selected = self.selected_pubkeys();
        self.store
            .active_wallet_pubkey
            .as_deref()
            .and_then(|value| Pubkey::try_from(value.trim()).ok())
            .filter(|pubkey| selected.contains(pubkey))
            .or_else(|| selected.first().copied())
    }

    pub fn selected_pubkeys(&self) -> Vec<Pubkey> {
        self.store
            .selected_wallet_pubkeys
            .iter()
            .filter_map(|value| Pubkey::try_from(value.trim()).ok())
            .collect()
    }

    pub fn public_wallets(&self) -> Vec<ManagedWalletSummary> {
        self.store.public_wallets()
    }

    pub fn signer_for(&self, pubkey: &Pubkey) -> Option<Arc<Keypair>> {
        self.signers.get(pubkey).cloned()
    }

    pub fn active_signer(&self) -> Option<Arc<Keypair>> {
        self.active_pubkey()
            .and_then(|pubkey| self.signer_for(&pubkey))
    }

    pub fn label_for(&self, pubkey: &Pubkey) -> Option<&str> {
        self.store
            .wallets
            .iter()
            .find(|wallet| wallet.pubkey == pubkey.to_string())
            .map(|wallet| wallet.label.as_str())
    }

    pub fn wallet_count(&self) -> usize {
        self.store.wallets.len()
    }

    pub fn selected_count(&self) -> usize {
        self.store.selected_wallet_pubkeys.len()
    }
}

pub fn wallet_store_path() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var(MAMBA_WALLET_STORE_PATH_ENV) {
        let raw = raw.trim();
        if !raw.is_empty() {
            return Some(PathBuf::from(raw));
        }
    }

    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|value| PathBuf::from(value).join(".config")))?;

    Some(
        base.join(DEFAULT_CONFIG_SUBDIR)
            .join(DEFAULT_STORE_FILENAME),
    )
}

pub fn ensure_wallet_store_has_signer(
    signer: &Keypair,
    preferred_label: &str,
) -> anyhow::Result<Option<ManagedWalletSummary>> {
    let Some(path) = wallet_store_path() else {
        return Ok(None);
    };
    let mut store = ManagedWalletStore::load_or_default(&path)?;
    let (summary, changed) = store.ensure_stored_signer(signer, preferred_label)?;
    if changed {
        store.save(&path)?;
    }
    Ok(Some(summary))
}

pub fn render_wallet_export(
    wallets: &[ManagedWalletExportRecord],
    format: ManagedWalletExportFormat,
) -> anyhow::Result<String> {
    match format {
        ManagedWalletExportFormat::Json => {
            serde_json::to_string_pretty(wallets).context("serialize wallet export json")
        }
        ManagedWalletExportFormat::Text => Ok(wallets
            .iter()
            .map(|wallet| {
                format!(
                    "label={}\npubkey={}\nprivate_key_base58={}\ncreated_at_utc={}",
                    wallet.label, wallet.pubkey, wallet.secret_key_base58, wallet.created_at_utc
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")),
    }
}

pub fn write_wallet_export(
    path: &Path,
    wallets: &[ManagedWalletExportRecord],
    format: ManagedWalletExportFormat,
) -> anyhow::Result<()> {
    let rendered = render_wallet_export(wallets, format)?;
    write_secure_text_file(path, &rendered)
}

pub async fn build_wallet_transfer(
    rpc: &RpcClient,
    params: &WalletTransferBuildParams,
) -> anyhow::Result<WalletTransferBuildResponse> {
    let amount = params.amount.trim();
    if amount.is_empty() {
        bail!("amount is required");
    }

    match params.asset_kind {
        WalletTransferAssetKind::Sol => {
            build_sol_transfer(rpc, params.from, params.to, amount).await
        }
        WalletTransferAssetKind::Token => {
            let mint = params
                .mint
                .context("mint is required for token transfers")?;
            build_token_transfer(rpc, params.from, params.to, mint, amount).await
        }
    }
}

pub async fn preview_wallet_clean(
    rpc: &RpcClient,
    owner: Pubkey,
) -> anyhow::Result<WalletCleanPreview> {
    let spl_accounts = rpc.get_token_accounts_by_owner_with_commitment(
        &owner,
        TokenAccountsFilter::ProgramId(spl_token::id()),
        CommitmentConfig::confirmed(),
    );
    let token_2022_accounts = rpc.get_token_accounts_by_owner_with_commitment(
        &owner,
        TokenAccountsFilter::ProgramId(spl_token_2022::id()),
        CommitmentConfig::confirmed(),
    );
    let (spl_accounts, token_2022_accounts) = tokio::join!(spl_accounts, token_2022_accounts);

    let mut entries = Vec::new();
    let mut push_entries = |accounts: Vec<RpcKeyedAccount>| -> anyhow::Result<()> {
        for account in accounts {
            if let Some(entry) = wallet_clean_entry_from_rpc(&owner, account)? {
                entries.push(entry);
            }
        }
        Ok(())
    };
    push_entries(
        spl_accounts
            .context("getTokenAccountsByOwner(spl-token) failed")?
            .value,
    )?;
    push_entries(
        token_2022_accounts
            .context("getTokenAccountsByOwner(token-2022) failed")?
            .value,
    )?;

    entries.sort_by(|left, right| {
        wallet_clean_action_rank(left.action)
            .cmp(&wallet_clean_action_rank(right.action))
            .then(right.reclaim_lamports.cmp(&left.reclaim_lamports))
            .then(right.amount_raw.cmp(&left.amount_raw))
            .then(left.mint.cmp(&right.mint))
            .then(left.token_account.cmp(&right.token_account))
    });

    let cleanable_accounts = entries
        .iter()
        .filter(|entry| entry.action != WalletCleanActionKind::Skip)
        .count();
    let burn_accounts = entries
        .iter()
        .filter(|entry| entry.action == WalletCleanActionKind::BurnAndClose)
        .count();
    let close_only_accounts = entries
        .iter()
        .filter(|entry| entry.action == WalletCleanActionKind::CloseEmpty)
        .count();
    let unwrap_accounts = entries
        .iter()
        .filter(|entry| entry.action == WalletCleanActionKind::UnwrapWsol)
        .count();
    let blocked_accounts = entries
        .iter()
        .filter(|entry| entry.action == WalletCleanActionKind::Skip)
        .count();
    let total_reclaim_lamports = entries
        .iter()
        .filter(|entry| entry.action != WalletCleanActionKind::Skip)
        .map(|entry| entry.reclaim_lamports)
        .sum();

    Ok(WalletCleanPreview {
        owner: owner.to_string(),
        total_token_accounts: entries.len(),
        cleanable_accounts,
        burn_accounts,
        close_only_accounts,
        unwrap_accounts,
        blocked_accounts,
        total_reclaim_lamports,
        total_reclaim_sol: lamports_to_sol(total_reclaim_lamports),
        entries,
    })
}

pub async fn build_wallet_clean(
    rpc: &RpcClient,
    params: &WalletCleanBuildParams,
) -> anyhow::Result<WalletCleanBuild> {
    let preview = preview_wallet_clean(rpc, params.owner).await?;
    let selected_entries = select_wallet_clean_entries(&preview.entries, params)?;
    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("getLatestBlockhash failed")?;
    let required_signers = vec![params.owner.to_string()];

    let mut batches = Vec::new();
    let mut current_entries: Vec<WalletCleanEntry> = Vec::new();
    let mut current_instructions: Vec<Instruction> = Vec::new();
    let mut current_reclaim_lamports: u64 = 0;

    for entry in selected_entries {
        let entry_instructions = wallet_clean_instructions(params.owner, &entry)?;
        let mut candidate_instructions = current_instructions.clone();
        candidate_instructions.extend(entry_instructions.clone());
        let candidate_tx =
            compile_unsigned_v0_transaction(&params.owner, &candidate_instructions, blockhash)
                .context("compile wallet cleaner transaction")?;
        let candidate_wire_len = bincode::serialize(&candidate_tx)
            .context("serialize wallet cleaner transaction")?
            .len();

        if !current_instructions.is_empty() && candidate_wire_len > MAX_WALLET_CLEAN_BATCH_TX_BYTES
        {
            batches.push(build_wallet_clean_batch(
                params.owner,
                blockhash,
                &required_signers,
                &current_entries,
                &current_instructions,
                current_reclaim_lamports,
                batches.len(),
            )?);
            current_entries = vec![entry.clone()];
            current_instructions = entry_instructions;
            current_reclaim_lamports = entry.reclaim_lamports;
            continue;
        }

        if current_instructions.is_empty() && candidate_wire_len > MAX_WALLET_CLEAN_BATCH_TX_BYTES {
            bail!(
                "wallet clean transaction exceeds Solana max transaction size for account {} (len={}, max={})",
                entry.token_account,
                candidate_wire_len,
                MAX_WALLET_CLEAN_BATCH_TX_BYTES
            );
        }

        current_entries.push(entry);
        current_instructions = candidate_instructions;
        current_reclaim_lamports = current_reclaim_lamports.saturating_add(
            current_entries
                .last()
                .map(|item| item.reclaim_lamports)
                .unwrap_or_default(),
        );
    }

    if !current_entries.is_empty() {
        batches.push(build_wallet_clean_batch(
            params.owner,
            blockhash,
            &required_signers,
            &current_entries,
            &current_instructions,
            current_reclaim_lamports,
            batches.len(),
        )?);
    }

    let selected_account_count = batches.iter().map(|batch| batch.token_account_count).sum();
    let selected_reclaim_lamports = batches.iter().map(|batch| batch.reclaim_lamports).sum();

    Ok(WalletCleanBuild {
        owner: params.owner.to_string(),
        burn_nonzero: params.burn_nonzero,
        close_empty: params.close_empty,
        close_wsol: params.close_wsol,
        selected_account_count,
        selected_reclaim_lamports,
        selected_reclaim_sol: lamports_to_sol(selected_reclaim_lamports),
        preview,
        batches,
    })
}

async fn build_sol_transfer(
    rpc: &RpcClient,
    from: Pubkey,
    to: Pubkey,
    amount: &str,
) -> anyhow::Result<WalletTransferBuildResponse> {
    let lamports = parse_decimal_amount_to_u64(amount, 9)?;
    if lamports == 0 {
        bail!("amount must be greater than zero");
    }

    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("getLatestBlockhash failed")?;
    let instructions = vec![system_instruction::transfer(&from, &to, lamports)];
    let unsigned = compile_unsigned_v0_transaction(&from, &instructions, blockhash)?;

    Ok(WalletTransferBuildResponse {
        transaction: encode_transaction_base64(&unsigned)?,
        required_signers: vec![from.to_string()],
        derived_addresses: BTreeMap::new(),
        kind: "sol".to_string(),
        amount_input: amount.to_string(),
        amount_raw: lamports.to_string(),
        decimals: 9,
        mint: None,
        token_program: None,
        simulation: None,
    })
}

fn build_wallet_clean_batch(
    owner: Pubkey,
    blockhash: solana_program::hash::Hash,
    required_signers: &[String],
    entries: &[WalletCleanEntry],
    instructions: &[Instruction],
    reclaim_lamports: u64,
    batch_index: usize,
) -> anyhow::Result<WalletCleanBatch> {
    let tx = compile_unsigned_v0_transaction(&owner, instructions, blockhash)
        .context("compile wallet cleaner batch transaction")?;
    let wire_len = bincode::serialize(&tx)
        .context("serialize wallet cleaner batch transaction")?
        .len();
    if wire_len > MAX_WALLET_CLEAN_BATCH_TX_BYTES {
        bail!(
            "wallet cleaner batch transaction exceeds Solana max size (len={wire_len}, max={MAX_WALLET_CLEAN_BATCH_TX_BYTES})"
        );
    }
    Ok(WalletCleanBatch {
        batch_index,
        transaction: encode_transaction_base64(&tx)?,
        required_signers: required_signers.to_vec(),
        action_count: instructions.len(),
        token_account_count: entries.len(),
        reclaim_lamports,
        reclaim_sol: lamports_to_sol(reclaim_lamports),
        actions: entries.to_vec(),
        simulation: None,
    })
}

async fn build_token_transfer(
    rpc: &RpcClient,
    from: Pubkey,
    to: Pubkey,
    mint: Pubkey,
    amount: &str,
) -> anyhow::Result<WalletTransferBuildResponse> {
    let mint_account = rpc
        .get_account(&mint)
        .await
        .with_context(|| format!("get mint account failed for {mint}"))?;
    let (token_program, decimals) = token_program_and_decimals(&mint_account, &mint)?;
    let amount_raw = parse_decimal_amount_to_u64(amount, decimals)?;
    if amount_raw == 0 {
        bail!("amount must be greater than zero");
    }

    let source_ata = derive_associated_token_address(&from, &mint, &token_program);
    rpc.get_account(&source_ata)
        .await
        .with_context(|| format!("source token account not found for {source_ata}"))?;

    let destination_ata = derive_associated_token_address(&to, &mint, &token_program);
    let mut instructions: Vec<Instruction> = vec![create_associated_token_account_idempotent(
        &from,
        &to,
        &mint,
        &token_program,
    )];

    let transfer_ix = if token_program == spl_token::id() {
        spl_token::instruction::transfer_checked(
            &token_program,
            &source_ata,
            &mint,
            &destination_ata,
            &from,
            &[],
            amount_raw,
            decimals,
        )
        .context("build spl-token transfer_checked")?
    } else if token_program == spl_token_2022::id() {
        spl_token_2022::instruction::transfer_checked(
            &token_program,
            &source_ata,
            &mint,
            &destination_ata,
            &from,
            &[],
            amount_raw,
            decimals,
        )
        .context("build spl-token-2022 transfer_checked")?
    } else {
        bail!("unsupported token program for mint {mint}: {token_program}");
    };
    instructions.push(transfer_ix);

    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("getLatestBlockhash failed")?;
    let unsigned = compile_unsigned_v0_transaction(&from, &instructions, blockhash)?;

    let mut derived = BTreeMap::new();
    derived.insert("source_ata".to_string(), source_ata.to_string());
    derived.insert("destination_ata".to_string(), destination_ata.to_string());

    Ok(WalletTransferBuildResponse {
        transaction: encode_transaction_base64(&unsigned)?,
        required_signers: vec![from.to_string()],
        derived_addresses: derived,
        kind: "token".to_string(),
        amount_input: amount.to_string(),
        amount_raw: amount_raw.to_string(),
        decimals,
        mint: Some(mint.to_string()),
        token_program: Some(token_program.to_string()),
        simulation: None,
    })
}

fn wallet_clean_entry_from_rpc(
    owner: &Pubkey,
    account: RpcKeyedAccount,
) -> anyhow::Result<Option<WalletCleanEntry>> {
    let token_account = Pubkey::from_str(account.pubkey.trim())
        .with_context(|| format!("invalid token account pubkey: {}", account.pubkey.trim()))?;
    let token_program = Pubkey::from_str(account.account.owner.trim()).with_context(|| {
        format!(
            "invalid token program for token account {}: {}",
            token_account,
            account.account.owner.trim()
        )
    })?;
    let UiAccountData::Json(parsed) = account.account.data else {
        return Ok(None);
    };
    let info = parsed
        .parsed
        .get("info")
        .context("parsed token account missing info")?;
    let mint = parse_pubkey_value(info.get("mint"), "mint")?;
    let parsed_owner = parse_pubkey_value(info.get("owner"), "owner")?;
    let amount_info = info
        .get("tokenAmount")
        .context("parsed token account missing tokenAmount")?;
    let amount_raw = amount_info
        .get("amount")
        .and_then(Value::as_str)
        .context("parsed token account missing tokenAmount.amount")?
        .parse::<u64>()
        .with_context(|| format!("invalid token amount for token account {token_account}"))?;
    let decimals = amount_info
        .get("decimals")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .context("parsed token account missing tokenAmount.decimals")?;
    let amount_ui = parse_ui_amount_value(amount_info);
    let state = info
        .get("state")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let close_authority = parse_optional_pubkey_value(info.get("closeAuthority"));
    let is_native_flag = info
        .get("isNative")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_native_wsol = is_native_flag
        || spl_token::native_mint::check_id(&mint)
        || spl_token_2022::native_mint::check_id(&mint);
    let is_associated =
        derive_associated_token_address(owner, &mint, &token_program) == token_account;

    let (action, burn_required, skip_reason) = resolve_wallet_clean_action(
        owner,
        &parsed_owner,
        &token_program,
        &mint,
        amount_raw,
        is_native_wsol,
        state.as_deref(),
        close_authority.as_ref(),
    );

    Ok(Some(WalletCleanEntry {
        token_account: token_account.to_string(),
        mint: mint.to_string(),
        token_program: token_program.to_string(),
        owner: parsed_owner.to_string(),
        action,
        amount_raw,
        amount_ui,
        decimals,
        reclaim_lamports: account.account.lamports,
        reclaim_sol: lamports_to_sol(account.account.lamports),
        burn_required,
        is_associated,
        is_native_wsol,
        state,
        skip_reason,
    }))
}

fn resolve_wallet_clean_action(
    requested_owner: &Pubkey,
    parsed_owner: &Pubkey,
    token_program: &Pubkey,
    _mint: &Pubkey,
    amount_raw: u64,
    is_native_wsol: bool,
    state: Option<&str>,
    close_authority: Option<&Pubkey>,
) -> (WalletCleanActionKind, bool, Option<String>) {
    if parsed_owner != requested_owner {
        return (
            WalletCleanActionKind::Skip,
            false,
            Some(format!(
                "token account owner mismatch (expected {requested_owner} got {parsed_owner})"
            )),
        );
    }

    if *token_program != spl_token::id() && *token_program != spl_token_2022::id() {
        return (
            WalletCleanActionKind::Skip,
            false,
            Some(format!("unsupported token program: {token_program}")),
        );
    }

    if let Some(close_authority) = close_authority
        && close_authority != requested_owner
    {
        return (
            WalletCleanActionKind::Skip,
            false,
            Some(format!(
                "close authority mismatch (expected {requested_owner} got {close_authority})"
            )),
        );
    }

    if let Some(state) = state {
        let normalized = state.trim().to_ascii_lowercase();
        if normalized == "frozen" {
            return (
                WalletCleanActionKind::Skip,
                false,
                Some("token account is frozen".to_string()),
            );
        }
        if !normalized.is_empty() && normalized != "initialized" {
            return (
                WalletCleanActionKind::Skip,
                false,
                Some(format!("unsupported token account state: {normalized}")),
            );
        }
    }

    if is_native_wsol {
        return (WalletCleanActionKind::UnwrapWsol, false, None);
    }

    if amount_raw > 0 {
        (WalletCleanActionKind::BurnAndClose, true, None)
    } else {
        (WalletCleanActionKind::CloseEmpty, false, None)
    }
}

fn wallet_clean_action_rank(action: WalletCleanActionKind) -> u8 {
    match action {
        WalletCleanActionKind::UnwrapWsol => 0,
        WalletCleanActionKind::BurnAndClose => 1,
        WalletCleanActionKind::CloseEmpty => 2,
        WalletCleanActionKind::Skip => 3,
    }
}

fn select_wallet_clean_entries(
    entries: &[WalletCleanEntry],
    params: &WalletCleanBuildParams,
) -> anyhow::Result<Vec<WalletCleanEntry>> {
    let selected_accounts = params
        .token_accounts
        .iter()
        .map(Pubkey::to_string)
        .collect::<HashSet<_>>();
    let filter_by_account = !selected_accounts.is_empty();

    let mut selected = Vec::new();
    for entry in entries {
        if filter_by_account && !selected_accounts.contains(entry.token_account.as_str()) {
            continue;
        }
        if entry.action == WalletCleanActionKind::Skip {
            continue;
        }
        match entry.action {
            WalletCleanActionKind::UnwrapWsol if !params.close_wsol => continue,
            WalletCleanActionKind::CloseEmpty if !params.close_empty => continue,
            WalletCleanActionKind::BurnAndClose if !params.burn_nonzero => continue,
            WalletCleanActionKind::Skip => continue,
            _ => {}
        }
        selected.push(entry.clone());
    }

    if selected.is_empty() {
        bail!("no wallet cleaner actions matched the requested selection");
    }

    if filter_by_account {
        let selected_account_set = selected
            .iter()
            .map(|entry| entry.token_account.as_str())
            .collect::<HashSet<_>>();
        let missing = selected_accounts
            .iter()
            .filter(|token_account| !selected_account_set.contains(token_account.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!(
                "requested token accounts are not cleanable with current options: {}",
                missing.join(", ")
            );
        }
    }

    Ok(selected)
}

fn wallet_clean_instructions(
    owner: Pubkey,
    entry: &WalletCleanEntry,
) -> anyhow::Result<Vec<Instruction>> {
    let token_program = Pubkey::from_str(entry.token_program.trim()).with_context(|| {
        format!(
            "invalid token program for wallet cleaner entry {}",
            entry.token_account
        )
    })?;
    let token_account = Pubkey::from_str(entry.token_account.trim()).with_context(|| {
        format!(
            "invalid token account for wallet cleaner entry {}",
            entry.token_account
        )
    })?;
    let mint = Pubkey::from_str(entry.mint.trim())
        .with_context(|| format!("invalid mint for wallet cleaner entry {}", entry.mint))?;

    let mut instructions = Vec::new();
    if entry.action == WalletCleanActionKind::BurnAndClose {
        let burn_ix = if token_program == spl_token::id() {
            spl_token::instruction::burn_checked(
                &token_program,
                &token_account,
                &mint,
                &owner,
                &[],
                entry.amount_raw,
                entry.decimals,
            )
            .context("build spl-token burn_checked")?
        } else if token_program == spl_token_2022::id() {
            spl_token_2022::instruction::burn_checked(
                &token_program,
                &token_account,
                &mint,
                &owner,
                &[],
                entry.amount_raw,
                entry.decimals,
            )
            .context("build spl-token-2022 burn_checked")?
        } else {
            bail!("unsupported token program for wallet cleaner burn: {token_program}");
        };
        instructions.push(burn_ix);
    }

    instructions.push(
        spl_token_2022::instruction::close_account(
            &token_program,
            &token_account,
            &owner,
            &owner,
            &[],
        )
        .context("build close_account")?,
    );
    Ok(instructions)
}

fn parse_pubkey_value(value: Option<&Value>, label: &str) -> anyhow::Result<Pubkey> {
    let raw = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("parsed token account missing {label}"))?;
    Pubkey::from_str(raw).with_context(|| format!("invalid pubkey for {label}: {raw}"))
}

fn parse_optional_pubkey_value(value: Option<&Value>) -> Option<Pubkey> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|raw| Pubkey::from_str(raw).ok())
}

fn parse_ui_amount_value(amount_info: &Value) -> Option<f64> {
    amount_info
        .get("uiAmount")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .or_else(|| {
            amount_info
                .get("uiAmountString")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .and_then(|raw| raw.parse::<f64>().ok())
                .filter(|value| value.is_finite())
        })
}

fn lamports_to_sol(lamports: u64) -> f64 {
    (lamports as f64) / 1_000_000_000.0
}

fn parse_stored_signer(wallet: &ManagedWalletRecord) -> anyhow::Result<Keypair> {
    let bytes = bs58::decode(wallet.secret_key_base58.trim())
        .into_vec()
        .with_context(|| format!("decode stored secret key for {}", wallet.pubkey))?;
    let signer = Keypair::try_from(bytes.as_slice()).with_context(|| {
        format!(
            "stored secret key must encode 64 bytes for {}",
            wallet.pubkey
        )
    })?;
    let pubkey = signer.pubkey().to_string();
    if pubkey != wallet.pubkey {
        bail!(
            "stored wallet pubkey mismatch: record={} signer={pubkey}",
            wallet.pubkey
        );
    }
    Ok(signer)
}

fn write_secure_text_file(path: &Path, contents: &str) -> anyhow::Result<()> {
    let Some(parent) = path.parent() else {
        bail!("invalid wallet file path (no parent): {}", path.display());
    };

    fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    set_private_permissions(parent, 0o700)
        .with_context(|| format!("chmod 700 {}", parent.display()))?;

    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    set_private_permissions(&tmp, 0o600).with_context(|| format!("chmod 600 {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, mode: u32) -> anyhow::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set permissions {mode:o} on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _mode: u32) -> anyhow::Result<()> {
    Ok(())
}

fn token_program_and_decimals(
    account: &solana_account::Account,
    mint: &Pubkey,
) -> anyhow::Result<(Pubkey, u8)> {
    if account.owner == spl_token::id() {
        let parsed = SplMint::unpack_from_slice(&account.data)
            .with_context(|| format!("decode spl-token mint {mint}"))?;
        return Ok((spl_token::id(), parsed.decimals));
    }

    if account.owner == spl_token_2022::id() {
        let parsed = StateWithExtensions::<SplToken2022Mint>::unpack(&account.data)
            .with_context(|| format!("decode spl-token-2022 mint {mint}"))?;
        return Ok((spl_token_2022::id(), parsed.base.decimals));
    }

    bail!("unsupported mint owner for {mint}: {}", account.owner)
}

fn parse_decimal_amount_to_u64(raw: &str, decimals: u8) -> anyhow::Result<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("amount is required");
    }
    if trimmed.starts_with('-') {
        bail!("amount must be non-negative");
    }

    let (whole_raw, frac_raw) = match trimmed.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (trimmed, ""),
    };

    if !whole_raw.chars().all(|value| value.is_ascii_digit()) {
        bail!("invalid amount: {trimmed}");
    }
    if !frac_raw.chars().all(|value| value.is_ascii_digit()) {
        bail!("invalid amount: {trimmed}");
    }
    if frac_raw.len() > decimals as usize {
        bail!("amount has too many decimal places (max {decimals})");
    }

    let scale = 10u128.pow(u32::from(decimals));
    let whole = if whole_raw.is_empty() {
        0u128
    } else {
        whole_raw
            .parse::<u128>()
            .with_context(|| format!("invalid amount: {trimmed}"))?
    };
    let mut frac = if frac_raw.is_empty() {
        0u128
    } else {
        frac_raw
            .parse::<u128>()
            .with_context(|| format!("invalid amount: {trimmed}"))?
    };
    let missing_scale = decimals as usize - frac_raw.len();
    for _ in 0..missing_scale {
        frac *= 10;
    }

    let raw_amount = whole
        .checked_mul(scale)
        .and_then(|value| value.checked_add(frac))
        .context("amount overflow")?;
    u64::try_from(raw_amount).context("amount exceeds u64 range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_amount_parser_respects_scale() {
        assert_eq!(parse_decimal_amount_to_u64("1", 9).unwrap(), 1_000_000_000);
        assert_eq!(parse_decimal_amount_to_u64("1.23", 6).unwrap(), 1_230_000);
        assert_eq!(parse_decimal_amount_to_u64("0.000001", 6).unwrap(), 1);
    }

    #[test]
    fn decimal_amount_parser_rejects_precision_overflow() {
        let error = parse_decimal_amount_to_u64("0.0000001", 6).unwrap_err();
        assert!(error.to_string().contains("too many decimal places"));
    }

    #[test]
    fn wallet_store_normalizes_active_and_selected() {
        let pubkey = Keypair::new().pubkey().to_string();
        let mut store = ManagedWalletStore {
            version: 99,
            active_wallet_pubkey: Some("missing".to_string()),
            selected_wallet_pubkeys: vec!["missing".to_string()],
            wallets: vec![ManagedWalletRecord {
                pubkey: pubkey.clone(),
                label: "wallet-01".to_string(),
                secret_key_base58: bs58::encode(Keypair::new().to_bytes()).into_string(),
                created_at_utc: "2026-03-09T00:00:00Z".to_string(),
            }],
        };
        store.normalize();
        assert_eq!(store.version, STORE_VERSION);
        assert_eq!(store.active_wallet_pubkey, None);
        assert!(store.selected_wallet_pubkeys.is_empty());
    }

    #[test]
    fn ensure_stored_signer_inserts_and_selects_when_store_empty() {
        let signer = Keypair::new();
        let pubkey = signer.pubkey().to_string();
        let mut store = ManagedWalletStore::default();

        let (summary, changed) = store.ensure_stored_signer(&signer, "main").unwrap();

        assert!(changed);
        assert_eq!(summary.pubkey, pubkey);
        assert_eq!(summary.label, "main");
        assert_eq!(store.active_wallet_pubkey.as_deref(), Some(pubkey.as_str()));
        assert_eq!(store.selected_wallet_pubkeys, vec![pubkey.clone()]);
        assert_eq!(store.wallets.len(), 1);
        assert_eq!(
            store.wallets[0].secret_key_base58,
            bs58::encode(signer.to_bytes()).into_string()
        );
    }

    #[test]
    fn ensure_stored_signer_is_idempotent_for_existing_wallet() {
        let signer = Keypair::new();
        let pubkey = signer.pubkey().to_string();
        let mut store = ManagedWalletStore {
            version: STORE_VERSION,
            active_wallet_pubkey: Some(pubkey.clone()),
            selected_wallet_pubkeys: vec![pubkey.clone()],
            wallets: vec![ManagedWalletRecord {
                pubkey: pubkey.clone(),
                label: "main".to_string(),
                secret_key_base58: bs58::encode(signer.to_bytes()).into_string(),
                created_at_utc: "2026-04-01T00:00:00Z".to_string(),
            }],
        };

        let (summary, changed) = store.ensure_stored_signer(&signer, "main").unwrap();

        assert!(!changed);
        assert_eq!(summary.pubkey, pubkey);
        assert_eq!(store.wallets.len(), 1);
        assert_eq!(store.wallets[0].label, "main");
    }

    #[test]
    fn ensure_stored_signer_uses_unique_label_when_main_exists() {
        let existing_signer = Keypair::new();
        let signer = Keypair::new();
        let mut store = ManagedWalletStore {
            version: STORE_VERSION,
            active_wallet_pubkey: None,
            selected_wallet_pubkeys: Vec::new(),
            wallets: vec![ManagedWalletRecord {
                pubkey: existing_signer.pubkey().to_string(),
                label: "main".to_string(),
                secret_key_base58: bs58::encode(existing_signer.to_bytes()).into_string(),
                created_at_utc: "2026-04-01T00:00:00Z".to_string(),
            }],
        };

        let (summary, changed) = store.ensure_stored_signer(&signer, "main").unwrap();

        assert!(changed);
        assert_ne!(summary.label, "main");
        assert!(summary.label.starts_with("main-"));
        assert_eq!(store.wallets.len(), 2);
    }

    #[test]
    fn export_wallets_returns_all_wallets_in_store_order() {
        let wallet_a = ManagedWalletRecord {
            pubkey: Keypair::new().pubkey().to_string(),
            label: "wallet-a".to_string(),
            secret_key_base58: bs58::encode(Keypair::new().to_bytes()).into_string(),
            created_at_utc: "2026-03-19T00:00:00Z".to_string(),
        };
        let wallet_b = ManagedWalletRecord {
            pubkey: Keypair::new().pubkey().to_string(),
            label: "wallet-b".to_string(),
            secret_key_base58: bs58::encode(Keypair::new().to_bytes()).into_string(),
            created_at_utc: "2026-03-19T00:01:00Z".to_string(),
        };
        let store = ManagedWalletStore {
            version: STORE_VERSION,
            active_wallet_pubkey: None,
            selected_wallet_pubkeys: Vec::new(),
            wallets: vec![wallet_a.clone(), wallet_b.clone()],
        };

        let exported = store.export_wallets(&[]).unwrap();
        assert_eq!(
            exported,
            vec![
                ManagedWalletExportRecord::from(&wallet_a),
                ManagedWalletExportRecord::from(&wallet_b),
            ]
        );
    }

    #[test]
    fn export_wallets_resolves_pubkeys_and_unique_labels() {
        let wallet_a = ManagedWalletRecord {
            pubkey: Keypair::new().pubkey().to_string(),
            label: "wallet-a".to_string(),
            secret_key_base58: bs58::encode(Keypair::new().to_bytes()).into_string(),
            created_at_utc: "2026-03-19T00:00:00Z".to_string(),
        };
        let wallet_b = ManagedWalletRecord {
            pubkey: Keypair::new().pubkey().to_string(),
            label: "wallet-b".to_string(),
            secret_key_base58: bs58::encode(Keypair::new().to_bytes()).into_string(),
            created_at_utc: "2026-03-19T00:01:00Z".to_string(),
        };
        let store = ManagedWalletStore {
            version: STORE_VERSION,
            active_wallet_pubkey: None,
            selected_wallet_pubkeys: Vec::new(),
            wallets: vec![wallet_a.clone(), wallet_b.clone()],
        };

        let exported = store
            .export_wallets(&vec![wallet_b.label.clone(), wallet_a.pubkey.clone()])
            .unwrap();
        assert_eq!(
            exported,
            vec![
                ManagedWalletExportRecord::from(&wallet_b),
                ManagedWalletExportRecord::from(&wallet_a),
            ]
        );
    }

    #[test]
    fn export_wallets_rejects_ambiguous_labels() {
        let store = ManagedWalletStore {
            version: STORE_VERSION,
            active_wallet_pubkey: None,
            selected_wallet_pubkeys: Vec::new(),
            wallets: vec![
                ManagedWalletRecord {
                    pubkey: Keypair::new().pubkey().to_string(),
                    label: "shared".to_string(),
                    secret_key_base58: bs58::encode(Keypair::new().to_bytes()).into_string(),
                    created_at_utc: "2026-03-19T00:00:00Z".to_string(),
                },
                ManagedWalletRecord {
                    pubkey: Keypair::new().pubkey().to_string(),
                    label: "shared".to_string(),
                    secret_key_base58: bs58::encode(Keypair::new().to_bytes()).into_string(),
                    created_at_utc: "2026-03-19T00:01:00Z".to_string(),
                },
            ],
        };

        let error = store
            .export_wallets(&vec!["shared".to_string()])
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn render_wallet_export_supports_json_and_text() {
        let wallets = vec![ManagedWalletExportRecord {
            pubkey: "pubkey-1".to_string(),
            label: "wallet-01".to_string(),
            secret_key_base58: "secret-1".to_string(),
            created_at_utc: "2026-03-19T00:00:00Z".to_string(),
        }];

        let json = render_wallet_export(&wallets, ManagedWalletExportFormat::Json).unwrap();
        let decoded: Vec<ManagedWalletExportRecord> = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, wallets);

        let text = render_wallet_export(&wallets, ManagedWalletExportFormat::Text).unwrap();
        assert!(text.contains("label=wallet-01"));
        assert!(text.contains("private_key_base58=secret-1"));
    }
}
