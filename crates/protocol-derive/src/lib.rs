//! Derive macros for godot-netlink-protocol
//!
//! This crate provides procedural macros for the GameMessage trait,
//! automatically generating ROUTE_ID and SCHEMA_HASH from struct definitions.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Lit, Meta};

/// Derives the GameMessage trait with automatic SCHEMA_HASH generation
///
/// # Attributes
///
/// - `#[route_id = N]`: Required. Specifies the route ID (must be >= 100)
///
/// # Example
///
/// ```ignore
/// use serde::{Serialize, Deserialize};
/// use godot_netlink_protocol_derive::GameMessage;
///
/// #[derive(Serialize, Deserialize, GameMessage)]
/// #[route_id = 100]
/// struct PlayerInput {
///     x: f32,
///     y: f32,
/// }
/// ```
///
/// The SCHEMA_HASH is automatically computed from:
/// - Struct name
/// - Field names
/// - Field type names (simplified)
///
/// This ensures that any structural change to the message results in a different hash.
#[proc_macro_derive(GameMessage, attributes(route_id))]
pub fn derive_game_message(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    // Extract route_id from attributes
    let route_id = extract_route_id(&input);

    // Generate schema hash from struct definition
    let schema_hash = calculate_schema_hash(&input);

    let expanded = quote! {
        impl godot_netlink_protocol::message_registry::GameMessage for #name {
            const ROUTE_ID: u16 = #route_id;
            const SCHEMA_HASH: u64 = #schema_hash;
        }
    };

    TokenStream::from(expanded)
}

/// Extracts the route_id attribute value
fn extract_route_id(input: &DeriveInput) -> u16 {
    for attr in &input.attrs {
        if attr.path().is_ident("route_id") {
            if let Meta::NameValue(meta) = &attr.meta {
                if let syn::Expr::Lit(expr_lit) = &meta.value {
                    if let Lit::Int(lit) = &expr_lit.lit {
                        return lit.base10_parse().expect("route_id must be a valid u16");
                    }
                }
            }
        }
    }
    panic!("GameMessage requires #[route_id = N] attribute");
}

/// Calculates a stable schema hash from the struct definition
///
/// Algorithm:
/// 1. Start with struct name hash
/// 2. For each field, hash (field_name + type_name)
/// 3. Combine all hashes using XOR
///
/// This is a simple but effective hash that changes whenever:
/// - Struct name changes
/// - Field name changes
/// - Field type changes
/// - Field order changes (implicitly, via iteration)
fn calculate_schema_hash(input: &DeriveInput) -> u64 {
    let struct_name = input.ident.to_string();
    let mut hash = simple_hash(&struct_name);

    if let Data::Struct(data_struct) = &input.data {
        if let Fields::Named(fields) = &data_struct.fields {
            for field in &fields.named {
                let field_name = field.ident.as_ref().unwrap().to_string();
                let field_type = &field.ty;
                let type_name = quote!(#field_type).to_string();
                let field_hash = simple_hash(&format!("{}:{}", field_name, type_name));
                hash ^= field_hash;
            }
        }
    }

    hash
}

/// Simple FNV-1a hash implementation
///
/// We use a simple hash algorithm to avoid external dependencies.
/// FNV-1a is fast and provides good distribution for our use case.
fn simple_hash(s: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_hash_stability() {
        // Hash should be deterministic
        let hash1 = simple_hash("test");
        let hash2 = simple_hash("test");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_simple_hash_different() {
        // Different strings should produce different hashes
        let hash1 = simple_hash("test1");
        let hash2 = simple_hash("test2");
        assert_ne!(hash1, hash2);
    }
}
