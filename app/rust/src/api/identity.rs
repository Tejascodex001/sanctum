use sanctum_core::identity::Identity;

/// A fresh 32-byte secret. Dart stores it in the Android Keystore-backed
/// secure storage; it never leaves the device.
pub fn identity_new() -> Vec<u8> {
    Identity::generate().secret().to_vec()
}

pub fn identity_public_key(secret: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    let secret: [u8; 32] = secret
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret must be 32 bytes"))?;
    Ok(Identity::from_secret(secret).public_key().to_vec())
}

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}
