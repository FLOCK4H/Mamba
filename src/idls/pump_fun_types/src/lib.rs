#![allow(unexpected_cfgs, deprecated)]

use anchor_gen::generate_cpi_crate;
use anchor_lang::prelude::*;

generate_cpi_crate!("pump.json");
