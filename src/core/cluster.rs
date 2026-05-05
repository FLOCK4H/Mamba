use solana_program::pubkey::Pubkey;

pub const DEFAULT_DEVNET_WS_URL: &str = "wss://api.devnet.solana.com";
pub const DEFAULT_DEVNET_HTTP_URL: &str = "https://api.devnet.solana.com";
pub const DEFAULT_MAINNET_WS_URL: &str = "wss://api.mainnet-beta.solana.com";
pub const DEFAULT_MAINNET_HTTP_URL: &str = "https://api.mainnet-beta.solana.com";

pub const GENESIS_HASH_MAINNET_BETA: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
pub const GENESIS_HASH_DEVNET: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
pub const GENESIS_HASH_TESTNET: &str = "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY";

// Raydium Launchpad program ids differ per cluster (Raydium SDK v2 exports DEV_* constants).
pub const RAYDIUM_LAUNCHPAD_PROGRAM_ID_MAINNET: Pubkey =
    Pubkey::from_str_const("LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj");
pub const RAYDIUM_LAUNCHPAD_PROGRAM_ID_DEVNET: Pubkey =
    Pubkey::from_str_const("DRay6fNdQ5J82H7xV6uq2aV3mNrUZ1J4PgSKsWgptcm6");
pub const RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_MAINNET: Pubkey =
    Pubkey::from_str_const("WLHv2UAZm6z4KyaaELi5pjdbJh6RESMva1Rnn8pJVVh");
pub const RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_DEVNET: Pubkey =
    Pubkey::from_str_const("5xqNaZXX5eUi4p5HU4oz9i5QnwRNT2y6oN7yyn4qENeq");

// Raydium CLMM/CPMM/AMM v4 program ids differ per cluster (upstream repos gate via `feature = "devnet"`).
pub const RAYDIUM_CLMM_PROGRAM_ID_MAINNET: Pubkey =
    Pubkey::from_str_const("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");
pub const RAYDIUM_CLMM_PROGRAM_ID_DEVNET: Pubkey =
    Pubkey::from_str_const("DRayAUgENGQBKVaX8owNhgzkEDyoHTGVEGHVJT1E9pfH");

pub const RAYDIUM_CPMM_PROGRAM_ID_MAINNET: Pubkey =
    Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
pub const RAYDIUM_CPMM_PROGRAM_ID_DEVNET: Pubkey =
    Pubkey::from_str_const("DRaycpLY18LhpbydsBWbVJtxpNv9oXPgjRSfpF2bWpYb");
pub const RAYDIUM_CPMM_CREATE_POOL_FEE_RECEIVER_MAINNET: Pubkey =
    Pubkey::from_str_const("DNXgeM9EiiaAbaWvwjHj9fQQLAX5ZsfHyvmYUNRAdNC8");
pub const RAYDIUM_CPMM_CREATE_POOL_FEE_RECEIVER_DEVNET: Pubkey =
    Pubkey::from_str_const("3oE58BKVt8KuYkGxx8zBojugnymWmBiyafWgMrnb6eYy");

pub const RAYDIUM_AMM_V4_PROGRAM_ID_MAINNET: Pubkey =
    Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
pub const RAYDIUM_AMM_V4_PROGRAM_ID_DEVNET: Pubkey =
    Pubkey::from_str_const("DRaya7Kj3aMWQSy19kSjvmuwq9docCHofyP9kanQGaav");
pub const RAYDIUM_AMM_V4_OPENBOOK_PROGRAM_ID_MAINNET: Pubkey =
    Pubkey::from_str_const("srmqPvymJeFKQ4zGQed1GFppgkRHL9kaELCbyksJtPX");
pub const RAYDIUM_AMM_V4_OPENBOOK_PROGRAM_ID_DEVNET: Pubkey =
    Pubkey::from_str_const("EoTcMgcDRTJVZDMZWBoU6rhYHZfkNTVEAfz3uUJRcYGj");
pub const RAYDIUM_AMM_V4_CREATE_POOL_FEE_DESTINATION_MAINNET: Pubkey =
    Pubkey::from_str_const("7YttLkHDoNj9wyDur5pM1ejNaAvT9X4eqaYcHQqtj2G5");
pub const RAYDIUM_AMM_V4_CREATE_POOL_FEE_DESTINATION_DEVNET: Pubkey =
    Pubkey::from_str_const("9y8ENuuZ3b19quffx9hQvRVygG5ky6snHfRvGpuSfeJy");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolanaCluster {
    MainnetBeta,
    Devnet,
    Testnet,
    Unknown,
}

impl SolanaCluster {
    pub fn from_genesis_hash(hash: &str) -> Self {
        match hash.trim() {
            GENESIS_HASH_MAINNET_BETA => Self::MainnetBeta,
            GENESIS_HASH_DEVNET => Self::Devnet,
            GENESIS_HASH_TESTNET => Self::Testnet,
            _ => Self::Unknown,
        }
    }
}

pub fn raydium_launchpad_program_id(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_LAUNCHPAD_PROGRAM_ID_DEVNET,
        _ => RAYDIUM_LAUNCHPAD_PROGRAM_ID_MAINNET,
    }
}

pub fn raydium_launchpad_authority_pda(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_DEVNET,
        _ => RAYDIUM_LAUNCHPAD_AUTHORITY_PDA_MAINNET,
    }
}

pub fn raydium_clmm_program_id(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_CLMM_PROGRAM_ID_DEVNET,
        _ => RAYDIUM_CLMM_PROGRAM_ID_MAINNET,
    }
}

pub fn raydium_cpmm_program_id(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_CPMM_PROGRAM_ID_DEVNET,
        _ => RAYDIUM_CPMM_PROGRAM_ID_MAINNET,
    }
}

pub fn raydium_cpmm_create_pool_fee_receiver(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_CPMM_CREATE_POOL_FEE_RECEIVER_DEVNET,
        _ => RAYDIUM_CPMM_CREATE_POOL_FEE_RECEIVER_MAINNET,
    }
}

pub fn raydium_amm_v4_program_id(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_AMM_V4_PROGRAM_ID_DEVNET,
        _ => RAYDIUM_AMM_V4_PROGRAM_ID_MAINNET,
    }
}

pub fn raydium_amm_v4_openbook_program_id(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_AMM_V4_OPENBOOK_PROGRAM_ID_DEVNET,
        _ => RAYDIUM_AMM_V4_OPENBOOK_PROGRAM_ID_MAINNET,
    }
}

pub fn raydium_amm_v4_create_pool_fee_destination(cluster: SolanaCluster) -> Pubkey {
    match cluster {
        SolanaCluster::Devnet => RAYDIUM_AMM_V4_CREATE_POOL_FEE_DESTINATION_DEVNET,
        _ => RAYDIUM_AMM_V4_CREATE_POOL_FEE_DESTINATION_MAINNET,
    }
}
