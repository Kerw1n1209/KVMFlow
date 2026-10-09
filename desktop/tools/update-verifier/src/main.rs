use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};
use std::{error::Error, fs, path::Path};

fn decode(value: &str) -> Result<String, Box<dyn Error>> {
    Ok(String::from_utf8(STANDARD.decode(value.trim())?)?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 3 || args.len() % 2 != 1 {
        return Err("usage: update-verifier <tauri.conf.json> <package> <signature> [...]".into());
    }
    let config: serde_json::Value = serde_json::from_slice(&fs::read(&args[0])?)?;
    let encoded = config["plugins"]["updater"]["pubkey"]
        .as_str()
        .ok_or("missing updater public key")?;
    let key = PublicKey::decode(&decode(encoded)?)?;
    for pair in args[1..].chunks_exact(2) {
        let signature = Signature::decode(&decode(&fs::read_to_string(&pair[1])?)?)?;
        // Use the same verifier as Tauri, including the trusted comment.
        key.verify(&fs::read(&pair[0])?, &signature, true)?;
        let signed_version = signature
            .trusted_comment()
            .split('\t')
            .find_map(|field| field.strip_prefix("version:"));
        if signed_version != config["version"].as_str() {
            return Err("signed package version does not match client release version".into());
        }
        println!(
            "Verified updater signature: {}",
            Path::new(&pair[0]).display()
        );
    }
    Ok(())
}
