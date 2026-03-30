use anyhow::{Result, bail};
use pbkdf2::pbkdf2_hmac;
use sha2::{Digest as _, Sha256};

pub(crate) fn encrypt_password(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    protocol: &str,
) -> Result<Vec<u8>> {
    let mut password_material = Sha256::digest(password).to_vec();
    match protocol {
        "s2k" => {}
        "s2k_fo" => {
            password_material = hex_lower(&password_material).into_bytes();
        }
        other => bail!("unsupported Apple SRP protocol `{other}`"),
    }

    let mut encrypted = vec![0u8; 32];
    pbkdf2_hmac::<Sha256>(&password_material, salt, iterations, &mut encrypted);
    Ok(encrypted)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
