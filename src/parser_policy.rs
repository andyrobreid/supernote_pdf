use anyhow::{Result, bail};
use clap::ValueEnum;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum ParserPolicy {
    Strict,
    Loose,
}

pub fn validate_signature(signature: &str, policy: ParserPolicy) -> Result<()> {
    if is_supported_signature(signature) {
        return Ok(());
    }

    if policy == ParserPolicy::Loose {
        eprintln!(
            "Warning: unsupported signature '{}' detected; continuing due to --policy loose.",
            signature
        );
        return Ok(());
    }

    bail!(
        "Unsupported note signature '{}'. Re-run with --policy loose to attempt best-effort parsing.",
        signature
    );
}

fn is_supported_signature(signature: &str) -> bool {
    // Known X-series signatures from observed firmware versions.
    const KNOWN_SIGNATURES: [&str; 11] = [
        "SN_FILE_VER_20200001",
        "SN_FILE_VER_20200005",
        "SN_FILE_VER_20200006",
        "SN_FILE_VER_20200007",
        "SN_FILE_VER_20200008",
        "SN_FILE_VER_20210009",
        "SN_FILE_VER_20210010",
        "SN_FILE_VER_20220011",
        "SN_FILE_VER_20220013",
        "SN_FILE_VER_20230014",
        "SN_FILE_VER_20230015",
    ];
    KNOWN_SIGNATURES.contains(&signature)
}
