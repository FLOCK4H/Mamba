use {
    serde::{Deserialize, Serialize},
    solana_program::pubkey::Pubkey,
};

pub mod blox;
pub mod helius;
pub mod jito;
pub mod nextblock;
pub mod temporal;
pub mod zero_slot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SwqosProvider {
    #[default]
    Jito,
    Helius,
    NextBlock,
    ZeroSlot,
    Temporal,
    Bloxroute,
}

impl SwqosProvider {
    pub const ALL: [Self; 6] = [
        Self::Jito,
        Self::Helius,
        Self::NextBlock,
        Self::ZeroSlot,
        Self::Temporal,
        Self::Bloxroute,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Jito => "jito",
            Self::Helius => "helius",
            Self::NextBlock => "nextblock",
            Self::ZeroSlot => "zero_slot",
            Self::Temporal => "temporal",
            Self::Bloxroute => "bloxroute",
        }
    }

    pub fn requires_api_key(self) -> bool {
        matches!(
            self,
            Self::NextBlock | Self::ZeroSlot | Self::Temporal | Self::Bloxroute
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SWQoSettings {
    pub provider: SwqosProvider,
    pub jito_key: Option<String>,
    pub nextblock_key: String,
    pub zero_slot_key: String,
    pub temporal_key: String,
    pub blox_key: String,
    pub tip_lamports: u64,
    pub nonce_account: Option<String>,
}

impl SWQoSettings {
    pub fn active_provider_key(&self) -> Option<&str> {
        match self.provider {
            SwqosProvider::Jito => self
                .jito_key
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
            SwqosProvider::Helius => None,
            SwqosProvider::NextBlock => {
                let value = self.nextblock_key.trim();
                (!value.is_empty()).then_some(value)
            }
            SwqosProvider::ZeroSlot => {
                let value = self.zero_slot_key.trim();
                (!value.is_empty()).then_some(value)
            }
            SwqosProvider::Temporal => {
                let value = self.temporal_key.trim();
                (!value.is_empty()).then_some(value)
            }
            SwqosProvider::Bloxroute => {
                let value = self.blox_key.trim();
                (!value.is_empty()).then_some(value)
            }
        }
    }
}

pub fn tip_account_for_provider(provider: SwqosProvider) -> Pubkey {
    match provider {
        SwqosProvider::Jito => jito::JITO_TIP_ACCS[0],
        SwqosProvider::Helius => helius::HELIUS_TIP_ACCS[0],
        SwqosProvider::NextBlock => nextblock::NB_TIP_ACCS[0],
        SwqosProvider::ZeroSlot => zero_slot::ZERO_SLOT_TIP_ACCS[0],
        SwqosProvider::Temporal => temporal::TEMPORAL_TIP_ACCS[0],
        SwqosProvider::Bloxroute => blox::BLOX_TIP_ACCS[0],
    }
}
