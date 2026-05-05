#![allow(
    ambiguous_glob_imports,
    ambiguous_glob_reexports,
    deprecated,
    unexpected_cfgs
)]

use anchor_gen::generate_cpi_crate;
use anchor_lang::prelude::*;

generate_cpi_crate!("../../../external/upstreams/meteora_dlmm_sdk/idls/dlmm.json");
