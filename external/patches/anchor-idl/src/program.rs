use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    path::PathBuf,
};

use darling::{util::PathList, FromMeta};
use heck::ToPascalCase;
use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::{
    generate_accounts, generate_events, generate_ix_handlers, generate_ix_structs,
    generate_typedefs, GEN_VERSION,
};

#[derive(Default, FromMeta)]
pub struct GeneratorOptions {
    /// Path to the IDL.
    pub idl_path: String,
    /// List of types to skip from generation. These should be provided by the caller instead.
    pub skip: Option<PathList>,
    /// List of zero copy structs.
    pub zero_copy: Option<PathList>,
    /// List of `repr(packed)` structs.
    pub packed: Option<PathList>,
}

fn path_list_to_string(list: Option<&PathList>) -> HashSet<String> {
    list.map(|el| {
        el.iter()
            .map(|el| el.get_ident().unwrap().to_string())
            .collect()
    })
    .unwrap_or_default()
}

impl GeneratorOptions {
    pub fn to_generator(&self) -> Generator {
        let cargo_manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        let path = PathBuf::from(cargo_manifest_dir).join(&self.idl_path);
        let idl_contents = fs::read_to_string(&path).unwrap();
        let idl: anchor_lang_idl_spec::Idl = serde_json::from_str(&idl_contents).unwrap();

        let skip = path_list_to_string(self.skip.as_ref());
        let zero_copy = path_list_to_string(self.zero_copy.as_ref());
        let packed = path_list_to_string(self.packed.as_ref());

        let all_type_names = idl
            .accounts
            .iter()
            .map(|a| a.name.clone())
            .chain(idl.types.iter().map(|t| t.name.clone()))
            .collect::<HashSet<_>>();

        let mut struct_opts: BTreeMap<String, StructOpts> = BTreeMap::new();
        all_type_names.iter().for_each(|name| {
            struct_opts.insert(
                name.to_string(),
                StructOpts {
                    skip: skip.contains(name),
                    zero_copy: zero_copy.contains(name),
                    packed: packed.contains(name),
                },
            );
        });

        Generator { idl, struct_opts }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StructOpts {
    pub skip: bool,
    pub packed: bool,
    pub zero_copy: bool,
}

pub struct Generator {
    pub idl: anchor_lang_idl_spec::Idl,
    pub struct_opts: BTreeMap<String, StructOpts>,
}

impl Generator {
    pub fn generate_cpi_interface(&self) -> TokenStream {
        let idl = &self.idl;
        let program_name: Ident = format_ident!("{}", idl.metadata.name);

        let accounts = generate_accounts(&idl.types, &idl.accounts, &self.struct_opts);
        let events = generate_events(&idl.events, &idl.types, &self.struct_opts);
        let typedefs = generate_typedefs(&idl.types, &self.struct_opts);
        let ix_handlers = generate_ix_handlers(&idl.instructions);
        let ix_structs = generate_ix_structs(&idl.instructions);

        let docs = format!(
        " Anchor CPI crate generated from {} v{} using [anchor-gen](https://crates.io/crates/anchor-gen) v{}.",
        &idl.metadata.name,
        &idl.metadata.version,
        &GEN_VERSION.unwrap_or("unknown")
    );

        let address = idl.address.clone();
        let ix_account_names = idl
            .instructions
            .iter()
            .map(|ix| ix.name.to_pascal_case())
            .collect::<HashSet<_>>();
        let state_account_names = idl
            .accounts
            .iter()
            .map(|acc| acc.name.clone())
            .collect::<HashSet<_>>();
        let event_names = idl
            .events
            .iter()
            .map(|event| event.name.clone())
            .collect::<HashSet<_>>();

        let state_exports = idl
            .accounts
            .iter()
            .filter(|acc| {
                !self
                    .struct_opts
                    .get(&acc.name)
                    .copied()
                    .unwrap_or_default()
                    .skip
            })
            .filter(|acc| !ix_account_names.contains(&acc.name))
            .map(|acc| format_ident!("{}", acc.name))
            .collect::<Vec<_>>();

        let typedef_exports = idl
            .types
            .iter()
            .filter(|typedef| {
                !self
                    .struct_opts
                    .get(&typedef.name)
                    .copied()
                    .unwrap_or_default()
                    .skip
            })
            // Account typedefs are exported via `state`; don't double-export.
            .filter(|typedef| !state_account_names.contains(&typedef.name))
            // Events are generated under `events`; don't export their backing struct typedefs.
            .filter(|typedef| !event_names.contains(&typedef.name))
            // Avoid name collisions with instruction account structs.
            .filter(|typedef| !ix_account_names.contains(&typedef.name))
            .map(|typedef| format_ident!("{}", typedef.name))
            .collect::<Vec<_>>();

        let state_reexports = if state_exports.is_empty() {
            quote! {}
        } else {
            quote! {
                pub use state::{ #(#state_exports),* };
            }
        };

        let typedef_reexports = if typedef_exports.is_empty() {
            quote! {}
        } else {
            quote! {
                pub use typedefs::{ #(#typedef_exports),* };
            }
        };

        quote! {
            use anchor_lang::prelude::*;

            declare_id!(#address);

            pub mod typedefs {
                //! User-defined types.
                use super::*;
                #typedefs
            }

            pub mod state {
                //! Structs of accounts which hold state.
                use super::*;
                #accounts
            }

            pub mod events {
                //! Structs of events generated by program.
                use super::*;
                #events
            }

            pub mod ix_accounts {
                //! Accounts used in instructions.
                use super::*;
                #ix_structs
            }

            use ix_accounts::*;
            #state_reexports
            #typedef_reexports

            #[program]
            pub mod #program_name {
                #![doc = #docs]

                use super::*;
                #ix_handlers
            }
        }
    }
}
