use crate::cli::LogFormat;
use crate::error::{Error, ErrorCode, Result};
use sha2::{Digest, Sha256};
use tracing_subscriber::EnvFilter;

pub fn init(format: LogFormat) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false);
    let result = match format {
        LogFormat::Text => builder.try_init(),
        LogFormat::Json => builder.json().try_init(),
    };
    result.map_err(|error| Error::new(ErrorCode::Internal, error.to_string()))
}

pub fn hash_identifier(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex_lower(&digest[..12])
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
