//! Invite-code decode (joiner side), for clipboard-detect join.
//!
//! Mirrors `src/invite.rs`'s `decode_invite_code` in the main tetron crate
//! and `tetron-webui`'s own copy of the same logic: an invite code is
//! `bs58(network_pubkey(32 bytes) || secret)`. That function lives in a
//! binary crate not meant to be depended on as a library, so it's
//! reimplemented here rather than imported.

pub fn decode_invite_code(code: &str) -> Result<(iroh::EndpointId, Vec<u8>), String> {
    let bytes = bs58::decode(code)
        .into_vec()
        .map_err(|e| format!("invalid invite code: {e}"))?;
    if bytes.len() <= 32 {
        return Err(format!(
            "invalid invite code: expected more than 32 bytes, got {}",
            bytes.len()
        ));
    }
    let net: [u8; 32] = bytes[0..32]
        .try_into()
        .map_err(|_| "invalid invite code: malformed network key".to_string())?;
    let secret = bytes[32..].to_vec();
    let network_pubkey = iroh::EndpointId::from_bytes(&net)
        .map_err(|e| format!("invalid network key in invite: {e}"))?;
    Ok((network_pubkey, secret))
}
