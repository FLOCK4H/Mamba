#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::panic,
        clippy::useless_conversion,
        clippy::unwrap_used,
        clippy::useless_vec
    )
)]
#![allow(
    clippy::cloned_ref_to_slice_refs,
    clippy::if_same_then_else,
    clippy::manual_clamp,
    clippy::needless_late_init,
    clippy::needless_lifetimes,
    clippy::ptr_arg,
    clippy::question_mark,
    clippy::too_many_arguments
)]

pub mod api;
pub mod compute_budget;
pub mod constants;
pub mod core;
pub mod dex;
pub mod gate;
pub mod handlers;
pub mod mcp;
pub mod swqos;
pub mod transfers;
pub mod utils;
