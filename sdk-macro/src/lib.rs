//! Procedural macros for the Nusantara smart contract SDK.
//!
//! Provides two macros:
//! - `#[program]`: attribute macro for program modules that generates entrypoint dispatch
//! - `#[derive(Accounts)]`: derive macro for account structs that generates deserialization

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, ItemMod, parse_macro_input};

/// Attribute macro for program modules.
///
/// Applied to a module containing public handler functions, this macro generates
/// an entrypoint dispatch function (`__dispatch`) that reads an 8-byte instruction
/// discriminator from the beginning of the instruction data and routes to the
/// corresponding handler.
///
/// Discriminators are computed as a simple XOR-fold of `"global:<fn_name>"` into
/// 8 bytes. The full SHA3-512 based discriminator can be used once the SDK ships
/// with a hashing utility, but the XOR-fold is deterministic and sufficient for
/// the initial implementation.
///
/// # Generated code
///
/// For each public function in the module, the macro generates:
/// - A `<FN_NAME>_DISCRIMINATOR` constant (`[u8; 8]`)
/// - A match arm in `__dispatch` that delegates to `mod_name::fn_name`
///
/// The generated `__dispatch` function has the signature:
/// ```ignore
/// pub fn __dispatch(
///     program_id: &Pubkey,
///     accounts: &[AccountInfo],
///     data: &[u8],
/// ) -> ProgramResult
/// ```
///
/// # Usage
///
/// ```ignore
/// #[program]
/// pub mod my_program {
///     use super::*;
///
///     pub fn initialize(
///         program_id: &crate::pubkey::Pubkey,
///         accounts: &[crate::account_info::AccountInfo],
///         ix_data: &[u8],
///     ) -> crate::program_error::ProgramResult {
///         Ok(())
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn program(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemMod);
    let mod_name = &input.ident;
    let vis = &input.vis;
    let attrs = &input.attrs;

    // Extract public functions from the module body.
    let mut functions = Vec::new();
    if let Some((_, items)) = &input.content {
        for item in items {
            if let syn::Item::Fn(func) = item
                && matches!(func.vis, syn::Visibility::Public(_))
            {
                functions.push(func.clone());
            }
        }
    }

    // Generate discriminator constants and dispatch match arms.
    let mut discriminator_consts = Vec::new();
    let mut match_arms = Vec::new();

    for func in &functions {
        let fn_name = &func.sig.ident;
        let fn_name_str = fn_name.to_string();
        let disc_name = quote::format_ident!("{}_DISCRIMINATOR", fn_name_str.to_uppercase());
        let global_str = format!("global:{fn_name_str}");

        // Compute discriminator at compile time via const evaluation.
        // XOR-fold the "global:<fn_name>" bytes into an 8-byte array.
        // This is deterministic and reproducible across compilations.
        discriminator_consts.push(quote! {
            pub const #disc_name: [u8; 8] = {
                let bytes = #global_str.as_bytes();
                let mut hash: [u8; 8] = [0u8; 8];
                let mut i = 0;
                while i < bytes.len() {
                    hash[i % 8] ^= bytes[i];
                    i += 1;
                }
                hash
            };
        });

        match_arms.push(quote! {
            d if d == #disc_name => {
                let ix_data = &data[8..];
                #mod_name::#fn_name(program_id, accounts, ix_data)
            }
        });
    }

    let mod_items = if let Some((_, items)) = &input.content {
        items.clone()
    } else {
        vec![]
    };

    let output = quote! {
        #(#discriminator_consts)*

        #(#attrs)*
        #vis mod #mod_name {
            #(#mod_items)*
        }

        /// Auto-generated entrypoint dispatch function.
        ///
        /// Reads the first 8 bytes of `data` as the instruction discriminator,
        /// then routes to the matching handler in the `#mod_name` module.
        /// Returns `ProgramError::InvalidInstructionData` if the discriminator
        /// is unrecognized or the data is too short.
        pub fn __dispatch(
            program_id: &crate::pubkey::Pubkey,
            accounts: &[crate::account_info::AccountInfo],
            data: &[u8],
        ) -> crate::program_error::ProgramResult {
            if data.len() < 8 {
                return Err(crate::program_error::ProgramError::InvalidInstructionData);
            }

            let mut discriminator = [0u8; 8];
            discriminator.copy_from_slice(&data[..8]);

            match discriminator {
                #(#match_arms,)*
                _ => Err(crate::program_error::ProgramError::InvalidInstructionData),
            }
        }
    };

    output.into()
}

/// Derive macro for account structs.
///
/// Generates a `try_from_accounts` method that maps a slice of `AccountInfo`
/// values to the struct's named fields by position. The slice must contain at
/// least as many elements as the struct has fields.
///
/// # Usage
///
/// ```ignore
/// #[derive(Accounts)]
/// pub struct Initialize<'info> {
///     pub payer: AccountInfo<'info>,
///     pub counter: AccountInfo<'info>,
/// }
/// ```
///
/// This generates:
/// ```ignore
/// impl<'info> Initialize<'info> {
///     pub fn try_from_accounts(
///         accounts: &'info [AccountInfo<'info>],
///     ) -> Result<Self, ProgramError> { ... }
/// }
/// ```
#[proc_macro_derive(Accounts, attributes(account))]
pub fn derive_accounts(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(fields) => &fields.named,
            _ => panic!("Accounts derive only supports structs with named fields"),
        },
        _ => panic!("Accounts derive only supports structs"),
    };

    let field_count = fields.len();
    let field_names: Vec<_> = fields.iter().map(|f| &f.ident).collect();
    let field_indices: Vec<usize> = (0..field_count).collect();

    // Extract lifetime parameters from the struct definition.
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let output = quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            /// Deserialize this account struct from a slice of `AccountInfo` values.
            ///
            /// Accounts are assigned to fields in declaration order. Returns
            /// `ProgramError::NotEnoughAccountKeys` if the slice is too short.
            pub fn try_from_accounts(
                accounts: &[crate::account_info::AccountInfo],
            ) -> Result<Self, crate::program_error::ProgramError> {
                if accounts.len() < #field_count {
                    return Err(crate::program_error::ProgramError::NotEnoughAccountKeys);
                }
                Ok(Self {
                    #(#field_names: accounts[#field_indices].clone(),)*
                })
            }
        }
    };

    output.into()
}
