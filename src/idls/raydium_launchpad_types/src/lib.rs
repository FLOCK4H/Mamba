#![allow(unexpected_cfgs, deprecated)]

use anchor_gen::generate_cpi_crate;
use anchor_lang::prelude::*;

generate_cpi_crate!(
    "../../../external/upstreams/raydium_docs/public/launchpad_creator_fee_upgrade/raydium_launchpad.json"
);

impl Default for typedefs::CurveParams {
    fn default() -> Self {
        Self::Constant {
            data: typedefs::ConstantCurve {
                supply: 0,
                total_base_sell: 0,
                total_quote_fund_raising: 0,
                migrate_type: 0,
            },
        }
    }
}

impl Default for typedefs::PlatformConfigParam {
    fn default() -> Self {
        Self::FeeWallet(Pubkey::default())
    }
}
