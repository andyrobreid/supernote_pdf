use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::Parser;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::{Rgba, RgbaImage, imageops};
use indicatif::ProgressBar;
use itertools::Itertools;
use lazy_static::lazy_static;
use rayon::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Input file (.note) or directory containing .note files
    #[arg(short, long)]
    input: PathBuf,

    /// Output file (.pdf/.txt) or directory
    #[arg(short, long)]
    output: PathBuf,

    /// Extract recognized text to .txt instead of rendering PDFs
    #[arg(long, default_value_t = false)]
    extract_text: bool,

    /// Generate both .pdf and .md outputs for each .note (cannot be used with --extract-text)
    #[arg(long, default_value_t = false, conflicts_with_all = ["extract_text", "markdown_only"])]
    pdf_and_markdown: bool,

    /// Generate markdown-only output (.md) from recognized text without generating PDFs
    #[arg(long, default_value_t = false, conflicts_with_all = ["extract_text", "pdf_and_markdown"])]
    markdown_only: bool,

    /// Normalize recognized text whitespace in markdown output: single newlines become spaces,
    /// paragraph breaks (double newlines) are preserved
    #[arg(long, default_value_t = false)]
    normalize_text_whitespace: bool,
}
const A5X_WIDTH: usize = 1404;
const A5X_HEIGHT: usize = 1872;
const A5X2_WIDTH: usize = 1920;
const A5X2_HEIGHT: usize = 2560;
const A6X2_WIDTH: usize = 1404;
const A6X2_HEIGHT: usize = 1872;

// precompile regex
lazy_static! {
    static ref METADATA_RE: Regex = Regex::new(r"<(?P<key>[^:]+?):(?P<value>.*?)>").unwrap();
    static ref TEXT_OBJECT_RE: Regex = Regex::new(r#"\{[^{}]*"type"\s*:\s*"Text"[^{}]*\}"#).unwrap();
    static ref LABEL_RE: Regex = Regex::new(r#""label"\s*:\s*"((?:\\.|[^"\\])*)""#).unwrap();
}

#[derive(Debug)]
pub struct Notebook {
    pub signature: String,
    pub pages: Vec<Page>,
    pub width: usize,
    pub height: usize,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug)]
pub struct Page {
    pub addr: u64,
    pub layers: Vec<Layer>,
    pub recognized_text: Option<String>,
}

#[derive(Debug, Default)]
pub struct Layer {
    pub key: String,
    pub protocol: String,
    pub bitmap_address: u64,
}

#[derive(Debug)]
struct PdfPageChunk {
    page_object: Vec<u8>,
    contents_object: Vec<u8>,
    image_object: Vec<u8>,
}

fn get_signature(file: &mut File) -> Result<String> {
    const SIGNATURE_OFFSET: u64 = 4;
    const SIGNATURE_LENGTH: usize = 20;

    // The `?` operator is used here. If `File::open` returns an `Err`, the `?`
    // will immediately stop this function and return that `Err` to the caller.
    // If it returns `Ok(file)`, it unwraps the value and assigns it to `file`.

    // Seek to the signature's starting position.
    file.seek(SeekFrom::Start(SIGNATURE_OFFSET))?;

    // Read the signature bytes.
    let mut signature_bytes = vec![0; SIGNATURE_LENGTH];
    file.read_exact(&mut signature_bytes)?;

    // Convert the bytes into a readable string.
    // since it is an anyhow result, "?" can propagate any type of error back in a generic way.
    let signature_string = String::from_utf8(signature_bytes)?;

    Ok(signature_string)
}

/// Reads a metadata block at a given address and parses it into a HashMap.
/// Metadata format is `<KEY1:VALUE1><KEY2:VALUE2>...`
fn read_block(file: &mut File, address: u64) -> Result<Vec<u8>> {
    if address == 0 {
        return Ok(Vec::new());
    }
    file.seek(SeekFrom::Start(address))?;

    let mut len_bytes = [0u8; 4];
    file.read_exact(&mut len_bytes)?;
    let block_len = u32::from_le_bytes(len_bytes) as usize;

    let mut content_bytes = vec![0; block_len];
    file.read_exact(&mut content_bytes)?;
    Ok(content_bytes)
}

fn parse_metadata_block(file: &mut File, address: u64) -> Result<HashMap<String, String>> {
    let content_bytes = read_block(file, address)?;
    if content_bytes.is_empty() {
        let empty: HashMap<String, String> = HashMap::new();
        return Ok(empty);
    }
    let content = String::from_utf8(content_bytes)?;

    // Use the regex to find all key-value pairs and collect them into a map.
    let map: HashMap<String, String> = METADATA_RE
        .captures_iter(&content)
        .map(|cap| {
            let key = cap.name("key").unwrap().as_str().to_string();
            let value = cap.name("value").unwrap().as_str().to_string();
            (key, value)
        })
        .collect();

    Ok(map)
}

fn decode_base64(input: &str) -> Result<Vec<u8>> {
    let mut cleaned = String::with_capacity(input.len());
    for ch in input.chars() {
        if !ch.is_whitespace() {
            cleaned.push(match ch {
                '-' => '+',
                '_' => '/',
                other => other,
            });
        }
    }

    while !cleaned.len().is_multiple_of(4) {
        cleaned.push('=');
    }

    let mut out = Vec::with_capacity((cleaned.len() / 4) * 3);
    for chunk in cleaned.as_bytes().chunks(4) {
        let mut vals = [0u8; 4];
        let mut pad = 0usize;
        for (i, &b) in chunk.iter().enumerate() {
            vals[i] = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    pad += 1;
                    0
                }
                _ => bail!("invalid base64 character in RECOGNTEXT payload"),
            };
        }

        let n = ((vals[0] as u32) << 18) | ((vals[1] as u32) << 12) | ((vals[2] as u32) << 6) | (vals[3] as u32);
        out.push(((n >> 16) & 0xff) as u8);
        if pad < 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if pad == 0 {
            out.push((n & 0xff) as u8);
        }
    }

    Ok(out)
}

fn unescape_json_string(input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        let escaped = chars.next()?;
        match escaped {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{0008}'),
            'f' => out.push('\u{000C}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let code_hex: String = chars.by_ref().take(4).collect();
                if code_hex.len() != 4 {
                    return None;
                }
                let code = u16::from_str_radix(&code_hex, 16).ok()?;
                let ch = char::from_u32(code as u32)?;
                out.push(ch);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn normalize_ocr_label_for_dedup(label: &str) -> String {
    label
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" .", ".")
        .replace(" ,", ",")
        .replace(" :", ":")
        .replace(" ;", ";")
        .replace(" !", "!")
        .replace(" ?", "?")
        .replace(" ]", "]")
        .replace("[ ", "[")
        .replace(" )", ")")
        .replace("( ", "(")
}

fn ocr_tokens(label: &str) -> Vec<String> {
    label
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

fn dice_similarity(tokens_a: &[String], tokens_b: &[String]) -> f64 {
    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }

    let mut freq_a: HashMap<&str, usize> = HashMap::new();
    for t in tokens_a {
        *freq_a.entry(t.as_str()).or_insert(0) += 1;
    }

    let mut intersection = 0usize;
    for t in tokens_b {
        if let Some(count) = freq_a.get_mut(t.as_str())
            && *count > 0
        {
            *count -= 1;
            intersection += 1;
        }
    }

    (2.0 * intersection as f64) / (tokens_a.len() + tokens_b.len()) as f64
}

fn is_near_duplicate_label(existing: &str, candidate: &str) -> bool {
    let existing_tokens = ocr_tokens(existing);
    let candidate_tokens = ocr_tokens(candidate);

    if existing_tokens.is_empty() || candidate_tokens.is_empty() {
        return false;
    }

    // quick exact tokenized match
    if existing_tokens == candidate_tokens {
        return true;
    }

    // Require leading phrase agreement to avoid deleting genuinely different notes
    let prefix = existing_tokens
        .iter()
        .zip(candidate_tokens.iter())
        .take(6)
        .filter(|(a, b)| a == b)
        .count();
    if prefix < 4 {
        return false;
    }

    let sim = dice_similarity(&existing_tokens, &candidate_tokens);
    let len_ratio = (existing_tokens.len().min(candidate_tokens.len()) as f64) / (existing_tokens.len().max(candidate_tokens.len()) as f64);

    sim >= 0.78 && len_ratio >= 0.6
}

fn dedupe_ocr_labels(labels: Vec<String>) -> Vec<String> {
    let mut deduped: Vec<String> = Vec::new();
    let mut seen_norm: Vec<String> = Vec::new();

    for label in labels {
        let norm = normalize_ocr_label_for_dedup(&label);
        if norm.is_empty() {
            continue;
        }
        if seen_norm.iter().any(|s| s == &norm) {
            continue;
        }

        if deduped.iter().any(|existing| is_near_duplicate_label(existing, &label)) {
            continue;
        }

        seen_norm.push(norm);
        deduped.push(label);
    }

    deduped
}

fn parse_recognition_payload(payload: &str) -> Result<Option<String>> {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let decoded = decode_base64(trimmed).context("failed to decode RECOGNTEXT payload")?;
    let json = String::from_utf8(decoded).context("RECOGNTEXT JSON payload is not valid UTF-8")?;

    let mut labels: Vec<String> = TEXT_OBJECT_RE
        .find_iter(&json)
        .filter_map(|obj| LABEL_RE.captures(obj.as_str()))
        .filter_map(|caps| caps.get(1).map(|m| m.as_str()))
        .filter_map(unescape_json_string)
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect();

    if labels.is_empty() {
        labels = LABEL_RE
            .captures_iter(&json)
            .filter_map(|caps| caps.get(1).map(|m| m.as_str()))
            .filter_map(unescape_json_string)
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty())
            .collect();
    }

    labels = dedupe_ocr_labels(labels);

    if labels.is_empty() { Ok(None) } else { Ok(Some(labels.join("\n"))) }
}

fn parse_recognized_text(file: &mut File, address: u64) -> Result<Option<String>> {
    let payload = read_block(file, address)?;
    if payload.is_empty() {
        return Ok(None);
    }
    let payload = String::from_utf8(payload).with_context(|| format!("RECOGNTEXT block at address {address} is not valid UTF-8"))?;
    parse_recognition_payload(&payload).with_context(|| format!("failed parsing RECOGNTEXT block at address {address}"))
}

/// Detects the device type and returns the appropriate width and height dimensions
fn detect_device_dimensions(file: &mut File, footer_map: &HashMap<String, String>) -> Result<(usize, usize)> {
    if let Some(header_addr_str) = footer_map.get("FILE_FEATURE")
        && let Ok(header_addr) = header_addr_str.parse::<u64>()
    {
        let header_map = parse_metadata_block(file, header_addr)?;
        if let Some(equipment) = header_map.get("APPLY_EQUIPMENT") {
            return match equipment.as_str() {
                // A5 X2 (Manta)
                "N5" => Ok((A5X2_WIDTH, A5X2_HEIGHT)),
                // A6 X2 (Nomad)
                "N6" => Ok((A6X2_WIDTH, A6X2_HEIGHT)),
                // A5X / A6X and fallback devices currently share this size.
                _ => Ok((A5X_WIDTH, A5X_HEIGHT)),
            };
        }
    }
    Ok((A5X_WIDTH, A5X_HEIGHT))
}

fn parse_notebook(file: &mut File) -> Result<Notebook> {
    let file_signature = get_signature(file)?;

    // Get footer address and map
    file.seek(SeekFrom::End(-4))?;
    let mut addr_bytes = [0u8; 4];
    file.read_exact(&mut addr_bytes)?;
    let footer_addr = u32::from_le_bytes(addr_bytes) as u64; // Convert the little-endian bytes to a u32, then cast to u64
    let footer_map = parse_metadata_block(file, footer_addr)?;

    // Detect device dimensions by parsing header
    let (width, height) = detect_device_dimensions(file, &footer_map)?;

    // get page addresses from the hashmap, sorted
    let page_addrs = footer_map
        .iter()
        .filter(|(k, _v)| k.starts_with("PAGE"))
        // .map(|(k, v)| (k.strip_prefix("PAGE").unwrap().parse::<u64>().unwrap(), v))
        .sorted_by_key(|(k, _v)| k.strip_prefix("PAGE").unwrap().parse::<u64>().unwrap())
        .map(|(_k, v)| v.parse::<u64>())
        .collect::<std::result::Result<Vec<u64>, _>>()?;

    // let page_map = parse_metadata_block(&mut file, *page_addrs.get(0).unwrap());
    // println!("{:?}", page_map);

    let mut pages: Vec<Page> = Vec::new();
    for addr in page_addrs {
        let page_map = parse_metadata_block(file, addr)?;
        let recognized_text = page_map
            .get("RECOGNTEXT")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|addr| *addr > 0)
            .map(|text_addr| parse_recognized_text(file, text_addr))
            .transpose()?
            .flatten();
        let layer_order = page_map
            .get("LAYERSEQ")
            .map(|s| s.split(',').map(String::from).collect())
            .unwrap_or_else(|| {
                // Default order if LAYERSEQ is missing
                vec![
                    "BGLAYER".to_string(),
                    "MAINLAYER".to_string(),
                    "LAYER1".to_string(),
                    "LAYER2".to_string(),
                    "LAYER3".to_string(),
                ]
            });
        let mut layers: Vec<Layer> = Vec::new();
        for layer_key in layer_order.iter() {
            // if page_map.contains_key(layer_key.as_str()) {
            if let Some(addr_str) = page_map.get(layer_key.as_str()) {
                let layer_addr = addr_str.parse::<u64>()?;
                let data = parse_metadata_block(file, layer_addr)?;
                layers.push(Layer {
                    key: layer_key.to_string(),
                    protocol: data.get("LAYERPROTOCOL").cloned().unwrap_or_default(),
                    bitmap_address: data.get("LAYERBITMAP").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                });
            }
        }
        pages.push(Page {
            addr,
            layers,
            recognized_text,
        });
    }

    Ok(Notebook {
        signature: file_signature,
        pages,
        width,
        height,
        metadata: footer_map,
    })
}

fn adjust_rle_tail_length(tail_length: u8, current_length: usize, total_length: usize) -> usize {
    let gap = total_length.saturating_sub(current_length);
    for shift in (0..8).rev() {
        let candidate = (((tail_length & 0x7f) as usize) + 1) << shift;
        if candidate <= gap {
            return candidate;
        }
    }
    0
}

/// Decodes a byte stream compressed with the RATTA_RLE algorithm.
fn decode_rle(compressed_data: &[u8], width: usize, height: usize) -> Result<Vec<u8>> {
    // Screen dimensions
    let expected_len = width * height;
    let mut decompressed = Vec::with_capacity(expected_len);

    let mut i = 0; // Our position in the compressed_data slice
    let mut holder: Option<(u8, u8)> = None; // State for multi-byte lengths

    while i < compressed_data.len() {
        // Ensure we can read a pair of bytes
        if i + 1 >= compressed_data.len() {
            break;
        }
        let color_code = compressed_data[i];
        let length_code = compressed_data[i + 1];
        i += 2; // Move to the next pair

        let mut emit_current = true;

        if let Some((prev_color_code, prev_length_code)) = holder.take() {
            // We are in the "holder" state from the previous iteration.
            if color_code == prev_color_code {
                // The colors match, so combine the lengths.
                let length = 1 + length_code as usize + (((prev_length_code & 0x7f) as usize + 1) << 7);
                // Combined run belongs to `color_code`.
                decompressed.extend(std::iter::repeat_n(color_code, length));
                emit_current = false;
            } else {
                // Colors don't match. First, process the held-over length.
                let held_length = ((prev_length_code & 0x7f) as usize + 1) << 7;
                decompressed.extend(std::iter::repeat_n(prev_color_code, held_length));
            }
        }

        if emit_current {
            let length: usize;
            if length_code == 0xff {
                // Special marker for a long run.
                length = 0x4000; // 16384
            } else if length_code & 0x80 != 0 {
                // Most significant bit is set. This is a multi-byte length marker.
                // Store and process at next loop iteration.
                holder = Some((color_code, length_code));
                continue;
            } else {
                // Standard case: length is just length_code + 1.
                length = length_code as usize + 1;
            }
            decompressed.extend(std::iter::repeat_n(color_code, length));
        }
    }

    // After the loop, check if there's a final item in the holder.
    // This can happen if the last block was a multi-byte marker.
    if let Some((color_code, length_code)) = holder {
        let tail_length = adjust_rle_tail_length(length_code, decompressed.len(), expected_len);
        if tail_length > 0 {
            decompressed.extend(std::iter::repeat_n(color_code, tail_length));
        }
    }

    // Final sanity check
    if decompressed.len() != expected_len {
        // In a real app, you might want a more robust way to handle this,
        // but for now, we can pad or truncate to the expected size.
        decompressed.resize(expected_len, 0x62); // Pad with transparent if too short
    }

    Ok(decompressed)
}

/// Maps a Supernote color codes to an RGBA pixel.
fn to_rgba(pixel_byte: u8) -> Rgba<u8> {
    match pixel_byte {
        // --- Core Colors ---
        0x61 => Rgba([0, 0, 0, 255]),       // Black
        0x65 => Rgba([255, 255, 255, 255]), // White
        0x62 => Rgba([0, 0, 0, 0]),         // Transparent (used for background layer)

        // --- Grays (and their aliases/compat codes) ---
        // Dark Gray
        0x63 | 0x9d | 0x9e => Rgba([0x9d, 0x9d, 0x9d, 255]),
        // Gray
        0x64 | 0xc9 | 0xca => Rgba([0xc9, 0xc9, 0xc9, 255]),

        // --- Handle all other bytes as anti-aliasing pixels ---
        _ => {
            // The byte value itself represents the grayscale intensity.
            // This renders the smooth edges of handwritten strokes.
            // this encoding is from the newer note format.
            Rgba([pixel_byte, pixel_byte, pixel_byte, 255])
        }
    }
}

fn stable_supernote_id(source_path: &str) -> String {
    let mut hash: i64 = 0;
    for byte in source_path.bytes() {
        hash = ((hash << 5) - hash) + byte as i64;
        hash &= 0xffff_ffff;
    }
    format!("sn-{:x}", hash.unsigned_abs())
}

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1} KB", bytes as f64 / 1024.0);
    }
    if bytes < 1024 * 1024 * 1024 {
        return format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0));
    }
    format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn format_timestamp_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(ms) = trimmed.parse::<i64>() {
            let seconds = if trimmed.len() >= 13 { ms / 1000 } else { ms };
            if let Some(dt) = DateTime::<Utc>::from_timestamp(seconds, 0) {
                return dt.to_rfc3339();
            }
        }
    }
    trimmed.to_string()
}

fn extract_supernote_timestamp(metadata: &HashMap<String, String>, candidates: &[&str]) -> Option<String> {
    let metadata_upper: HashMap<String, &String> = metadata.iter().map(|(k, v)| (k.to_ascii_uppercase(), v)).collect();

    for key in candidates {
        if let Some(value) = metadata_upper.get(&key.to_ascii_uppercase()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(format_timestamp_value(trimmed));
            }
        }
    }
    None
}

fn normalize_text_whitespace(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| {
            paragraph
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|paragraph| !paragraph.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn clean_duplicate_recognized_text(text: &str) -> String {
    let sections: Vec<String> = text.split("\n\n").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();

    let mut cleaned: Vec<String> = Vec::new();
    for section in sections {
        let normalized_section = section.split_whitespace().collect::<Vec<_>>().join(" ");

        let looks_like_word_per_line = {
            let lines: Vec<&str> = section.lines().collect();
            let single_word_lines = lines.iter().filter(|line| line.split_whitespace().count() <= 1).count();
            !lines.is_empty() && (single_word_lines as f64 / lines.len() as f64) > 0.8
        };

        let is_duplicate = cleaned.iter().any(|existing| {
            let normalized_existing = existing.split_whitespace().collect::<Vec<_>>().join(" ");
            normalized_existing.eq_ignore_ascii_case(&normalized_section)
        });

        if is_duplicate && looks_like_word_per_line {
            continue;
        }
        cleaned.push(section);
    }

    cleaned.join("\n\n")
}

fn trim_inline_near_duplicate_passage(text: &str) -> String {
    let mut spans: Vec<(usize, String)> = Vec::new();
    let mut current = String::new();
    let mut start_idx: Option<usize> = None;

    for (idx, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            if start_idx.is_none() {
                start_idx = Some(idx);
            }
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            spans.push((start_idx.unwrap_or(0), current.clone()));
            current.clear();
            start_idx = None;
        }
    }
    if !current.is_empty() {
        spans.push((start_idx.unwrap_or(0), current));
    }

    if spans.len() < 40 {
        return text.to_string();
    }

    let head: Vec<String> = spans.iter().take(4).map(|(_, t)| t.clone()).collect();
    if head.len() < 4 {
        return text.to_string();
    }

    for i in 20..(spans.len().saturating_sub(20)) {
        let cand: Vec<String> = spans.iter().skip(i).take(4).map(|(_, t)| t.clone()).collect();
        if cand != head {
            continue;
        }

        let prefix_tokens: Vec<String> = spans.iter().take(i).map(|(_, t)| t.clone()).collect();
        let suffix_tokens: Vec<String> = spans.iter().skip(i).map(|(_, t)| t.clone()).collect();

        let sim = dice_similarity(&prefix_tokens, &suffix_tokens);
        let len_ratio = (prefix_tokens.len().min(suffix_tokens.len()) as f64) / (prefix_tokens.len().max(suffix_tokens.len()) as f64);

        if sim >= 0.72 && len_ratio >= 0.45 {
            let cut_byte = spans[i].0;
            return text[..cut_byte].trim_end().to_string();
        }
    }

    text.to_string()
}

fn collect_recognized_text(notebook: &Notebook, normalize_whitespace: bool) -> String {
    let mut page_chunks = Vec::new();
    for (index, page) in notebook.pages.iter().enumerate() {
        if let Some(text) = page.recognized_text.as_deref() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let deduped = clean_duplicate_recognized_text(trimmed);
                if deduped.trim().is_empty() {
                    continue;
                }
                let deduped_inline = trim_inline_near_duplicate_passage(&deduped);
                let final_text = if normalize_whitespace {
                    normalize_text_whitespace(&deduped_inline)
                } else {
                    deduped_inline
                };
                page_chunks.push(format!("### Page {}\n\n{}", index + 1, final_text));
            }
        }
    }
    page_chunks.join("\n\n")
}

fn notebook_to_text(notebook: &Notebook, normalize_whitespace: bool) -> String {
    let text = collect_recognized_text(notebook, normalize_whitespace);
    if text.trim().is_empty() { String::new() } else { format!("{}\n", text) }
}

fn filesystem_timestamp_string(input_path: &Path, kind: &str) -> Option<String> {
    let metadata = std::fs::metadata(input_path).ok()?;
    let ts = match kind {
        "created" => metadata.created().ok()?,
        "modified" => metadata.modified().ok()?,
        _ => return None,
    };
    let dt: DateTime<Utc> = ts.into();
    Some(dt.to_rfc3339())
}

fn notebook_to_markdown(input_path: &Path, output_pdf_path: Option<&Path>, notebook: &Notebook, normalize_whitespace: bool) -> String {
    let title = input_path
        .file_stem()
        .or_else(|| input_path.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_string());

    let source_path = format!("/Note/{}.note", title);
    let supernote_id = stable_supernote_id(&source_path);
    let supernote_created = extract_supernote_timestamp(
        &notebook.metadata,
        &[
            "CREATE_TIME",
            "CREATEDATE",
            "CREATETIME",
            "FILE_CREATE_TIME",
            "FILE_CREATEDATE",
            "FILE_CREATETIME",
        ],
    )
    .unwrap_or_else(|| "unknown".to_string());
    let supernote_modified = extract_supernote_timestamp(
        &notebook.metadata,
        &[
            "LASTMODIFYDATE",
            "MODIFY_TIME",
            "UPDATETIME",
            "FILE_LASTMODIFYDATE",
            "FILE_MODIFY_TIME",
            "FILE_UPDATETIME",
        ],
    )
    .unwrap_or_else(|| "unknown".to_string());
    let file_created = filesystem_timestamp_string(input_path, "created").unwrap_or_else(|| "unknown".to_string());
    let file_modified = filesystem_timestamp_string(input_path, "modified").unwrap_or_else(|| "unknown".to_string());
    let file_size = std::fs::metadata(input_path)
        .map(|m| format_file_size(m.len()))
        .unwrap_or_else(|_| "unknown".to_string());

    let pdf_name = output_pdf_path.and_then(|path| path.file_name().map(|s| s.to_string_lossy().into_owned()));

    let recognized_text = collect_recognized_text(notebook, normalize_whitespace);
    let text_section = if recognized_text.trim().is_empty() {
        "_No recognized text found._".to_string()
    } else {
        recognized_text
    };

    let pdf_frontmatter = pdf_name.as_ref().map(|name| format!("pdf_attachment: {name}\n")).unwrap_or_default();

    let pdf_section = pdf_name
        .as_ref()
        .map(|name| format!("\n## PDF Attachment\n\n![[{name}]]\n"))
        .unwrap_or_default();

    format!(
        "---\nname: {title}\nsupernote_id: {supernote_id}\nsource: {source_path}\nsupernote_created: {supernote_created}\nsupernote_modified: {supernote_modified}\nfile_created: {file_created}\nfile_modified: {file_modified}\nsize: {file_size}\n{pdf_frontmatter}tags:\n  - supernote\n---\n\n# {title}\n\n## Note Information\n\n| Property | Value |\n|----------|-------|\n| **Source** | `{source_path}` |\n| **Supernote Created** | {supernote_created} |\n| **Supernote Modified** | {supernote_modified} |\n| **File Created** | {file_created} |\n| **File Modified** | {file_modified} |\n| **Size** | {file_size} |\n{pdf_section}\n## Text\n\n{text_section}\n"
    )
}

fn markdown_output_for_pdf_path(output_pdf_path: &Path) -> PathBuf {
    let mut markdown_path = output_pdf_path.to_path_buf();
    markdown_path.set_extension("md");
    markdown_path
}

fn extract_note_text(input_path: &Path, output_path: &Path, normalize_whitespace: bool) -> Result<()> {
    let notebook = {
        let mut file = File::open(input_path)?;
        parse_notebook(&mut file)?
    };
    let text = notebook_to_text(&notebook, normalize_whitespace);

    let out_file = File::create(output_path)?;
    let mut writer = BufWriter::new(out_file);
    writer.write_all(text.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn extract_note_markdown(input_path: &Path, output_path: &Path, output_pdf_path: Option<&Path>, normalize_whitespace: bool) -> Result<()> {
    let notebook = {
        let mut file = File::open(input_path)?;
        parse_notebook(&mut file)?
    };
    let markdown = notebook_to_markdown(input_path, output_pdf_path, &notebook, normalize_whitespace);

    let out_file = File::create(output_path)?;
    let mut writer = BufWriter::new(out_file);
    writer.write_all(markdown.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn convert_note_to_pdf(input_path: &Path, output_path: &Path) -> Result<()> {
    // file handle dropped outside this scope
    let notebook = {
        let mut file = File::open(input_path)?;
        parse_notebook(&mut file)?
    };

    let width = notebook.width;
    let height = notebook.height;

    let page_images = notebook
        .pages
        .par_iter()
        .map(|page| {
            let mut file = File::open(input_path)?;

            let mut base_canvas = RgbaImage::from_pixel(width as u32, height as u32, Rgba([255, 255, 255, 255]));

            for layer in page.layers.iter() {
                if layer.bitmap_address == 0 {
                    continue;
                } else if layer.protocol.as_str() == "RATTA_RLE" {
                    file.seek(SeekFrom::Start(layer.bitmap_address))?;
                    let mut len_bytes = [0u8; 4];
                    file.read_exact(&mut len_bytes)?;
                    let block_len = u32::from_le_bytes(len_bytes) as usize;
                    let mut compressed_data = vec![0; block_len];
                    file.read_exact(&mut compressed_data)?;
                    let pixel_data = decode_rle(&compressed_data, width, height)?;

                    let mut layer_image = RgbaImage::new(width as u32, height as u32);
                    for (i, &pixel_byte) in pixel_data.iter().enumerate() {
                        let x = (i % width) as u32;
                        let y = (i / width) as u32;
                        layer_image.put_pixel(x, y, to_rgba(pixel_byte));
                    }
                    imageops::overlay(&mut base_canvas, &layer_image, 0, 0);
                } else if layer.protocol.as_str() == "PNG" {
                    file.seek(SeekFrom::Start(layer.bitmap_address))?;
                    let mut len_bytes = [0u8; 4];
                    file.read_exact(&mut len_bytes)?;
                    let block_len = u32::from_le_bytes(len_bytes) as usize;

                    let mut png_bytes = vec![0; block_len];
                    file.read_exact(&mut png_bytes)?;
                    let png_image = image::load_from_memory(&png_bytes)?.to_rgba8();
                    imageops::overlay(&mut base_canvas, &png_image, 0, 0);
                }
            }

            Ok(base_canvas)
        })
        .collect::<Result<Vec<_>>>()?;
    let total_pages = page_images.len();
    let page_chunks: Vec<PdfPageChunk> = page_images
        .into_par_iter()
        .enumerate()
        .map(|(i, canvas)| {
            // Each page will use 3 objects: Page, Contents, Image
            let page_obj_id = (i * 3) + 3;
            let contents_obj_id = (i * 3) + 4;
            let image_obj_id = (i * 3) + 5;

            let dynamic_image = image::DynamicImage::ImageRgba8(canvas);

            let raw_pixels = dynamic_image.to_rgb8().into_raw();

            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&raw_pixels).unwrap();
            let compressed_pixels = encoder.finish().unwrap();

            let page_object = format!(
                "{} 0 obj\n<< /Type /Page\n   /Parent 2 0 R\n   /MediaBox [0 0 595 842]\n   /Contents {} 0 R\n   /Resources << /XObject << /Im1 {} 0 R >> >>\n>>\nendobj\n",
                page_obj_id,
                contents_obj_id,
                image_obj_id
            ).into_bytes();

            let contents = "q\n595 0 0 842 0 0 cm\n/Im1 Do\nQ\n";
            let contents_object = format!(
                "{} 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
                contents_obj_id,
                contents.len(),
                contents
            ).into_bytes();
            let image_header = format!(
                "{} 0 obj\n<< /Type /XObject\n   /Subtype /Image\n   /Width {}\n   /Height {}\n   /ColorSpace /DeviceRGB\n   /BitsPerComponent 8\n   /Filter /FlateDecode\n   /Length {} >>\nstream\n",
                image_obj_id,
                width,
                height,
                compressed_pixels.len()
            ).into_bytes();

            // Combine the header, the compressed data, and the footer for the image object
            let final_image_object = [
                image_header,
                compressed_pixels,
                b"\nendstream\nendobj\n".to_vec()
            ].concat();

            PdfPageChunk {
                page_object,
                contents_object,
                image_object: final_image_object,
            }
        })
        .collect();

    // Write everything to a file sequentially
    let out_file = File::create(output_path)?;
    let mut writer = BufWriter::new(out_file);
    let mut byte_offset = 0u64;
    let mut xref_offsets = vec![0u64; total_pages * 3 + 2]; // Room for all objects

    // Write PDF Header
    let header = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n"; // Header + binary comment
    writer.write_all(header)?;
    byte_offset += header.len() as u64;

    // Object 1: Catalog
    xref_offsets[0] = byte_offset;
    let catalog = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
    writer.write_all(catalog)?;
    byte_offset += catalog.len() as u64;

    // Object 2: The root Pages object
    xref_offsets[1] = byte_offset;
    let page_refs: String = (0..total_pages).map(|i| format!("{} 0 R", (i * 3) + 3)).collect::<Vec<_>>().join(" ");
    let pages_root = format!("2 0 obj\n<< /Type /Pages /Kids [ {} ] /Count {} >>\nendobj\n", page_refs, total_pages).into_bytes();
    writer.write_all(&pages_root)?;
    byte_offset += pages_root.len() as u64;

    // --- Write all the page chunks : cannot be parallelised ---
    for (i, chunk) in page_chunks.iter().enumerate() {
        let page_obj_id_idx = (i * 3) + 2;

        xref_offsets[page_obj_id_idx] = byte_offset;
        writer.write_all(&chunk.page_object)?;
        byte_offset += chunk.page_object.len() as u64;

        xref_offsets[page_obj_id_idx + 1] = byte_offset;
        writer.write_all(&chunk.contents_object)?;
        byte_offset += chunk.contents_object.len() as u64;

        xref_offsets[page_obj_id_idx + 2] = byte_offset;
        writer.write_all(&chunk.image_object)?;
        byte_offset += chunk.image_object.len() as u64;
    }

    // --- Write Cross-Reference Table and Trailer ---
    let xref_start_offset = byte_offset;
    writer.write_all(b"xref\n")?;
    writer.write_all(format!("0 {}\n", xref_offsets.len() + 1).as_bytes())?;
    writer.write_all(b"0000000000 65535 f \n")?; // XRef entry for object 0
    for offset in &xref_offsets {
        writer.write_all(format!("{:010} 00000 n \n", offset).as_bytes())?;
    }

    writer.write_all(b"trailer\n")?;
    writer.write_all(format!("<< /Size {} /Root 1 0 R >>\n", xref_offsets.len() + 1).as_bytes())?;
    writer.write_all(b"startxref\n")?;
    writer.write_all(format!("{}\n", xref_start_offset).as_bytes())?;
    writer.write_all(b"%%EOF\n")?;

    writer.flush()?;

    Ok(())
}

fn process_single_file(
    input_file: &Path,
    output_file: &Path,
    extract_text: bool,
    pdf_and_markdown: bool,
    markdown_only: bool,
    normalize_text_whitespace: bool,
) -> Result<()> {
    if input_file.extension().is_none_or(|s| s != "note") {
        bail!("Input file '{}' must have a .note extension.", input_file.display());
    }
    if output_file.is_dir() {
        bail!(
            "Input is a file, but output '{}' is a directory. Please specify an output file path.",
            output_file.display()
        );
    }
    let expected_output_ext = if extract_text {
        "txt"
    } else if markdown_only {
        "md"
    } else {
        "pdf"
    };
    if output_file.extension().is_none_or(|s| s != expected_output_ext) {
        bail!("Output file '{}' must have a .{} extension.", output_file.display(), expected_output_ext);
    }
    if output_file.exists() {
        bail!(
            "Output file '{}' already exists. Please remove it or choose a different name.",
            output_file.display()
        );
    }
    let markdown_file = if pdf_and_markdown {
        let markdown_file = markdown_output_for_pdf_path(output_file);
        if markdown_file.exists() {
            bail!(
                "Markdown output file '{}' already exists. Please remove it or choose a different output PDF path.",
                markdown_file.display()
            );
        }
        Some(markdown_file)
    } else {
        None
    };

    if extract_text {
        println!("Extracting recognized text from single file...");
    } else if pdf_and_markdown {
        println!("Converting single file and generating markdown...");
    } else if markdown_only {
        println!("Generating markdown from single file...");
    } else {
        println!("Converting single file...");
    }
    let start = Instant::now();
    let pb = ProgressBar::new_spinner();
    let action = if extract_text {
        "Extracting text from"
    } else if pdf_and_markdown {
        "Converting and extracting text from"
    } else if markdown_only {
        "Extracting markdown from"
    } else {
        "Converting"
    };
    pb.set_message(format!("{action} {}...", input_file.display()));

    if extract_text {
        extract_note_text(input_file, output_file, normalize_text_whitespace)?;
    } else if pdf_and_markdown {
        convert_note_to_pdf(input_file, output_file)?;
        extract_note_markdown(
            input_file,
            markdown_file
                .as_deref()
                .expect("markdown output path should be set when --pdf-and-markdown is enabled"),
            Some(output_file),
            normalize_text_whitespace,
        )?;
    } else if markdown_only {
        extract_note_markdown(input_file, output_file, None, normalize_text_whitespace)?;
    } else {
        convert_note_to_pdf(input_file, output_file)?;
    }

    let done_message = if extract_text {
        "Text extraction complete!"
    } else if pdf_and_markdown {
        "PDF and markdown generation complete!"
    } else if markdown_only {
        "Markdown generation complete!"
    } else {
        "Conversion complete!"
    };
    pb.finish_with_message(done_message);
    if let Some(markdown_file) = markdown_file {
        println!(
            "Successfully processed '{}' to '{}' and '{}' in {:?}",
            input_file.display(),
            output_file.display(),
            markdown_file.display(),
            start.elapsed()
        );
    } else {
        println!(
            "Successfully processed '{}' to '{}' in {:?}",
            input_file.display(),
            output_file.display(),
            start.elapsed()
        );
    }

    Ok(())
}

fn process_directory(
    input_dir: &Path,
    output_dir: &Path,
    extract_text: bool,
    pdf_and_markdown: bool,
    markdown_only: bool,
    normalize_text_whitespace: bool,
) -> Result<()> {
    if output_dir.is_file() {
        bail!(
            "Input is a directory, but output '{}' is a file. Please specify an output directory.",
            output_dir.display()
        );
    }

    if output_dir.exists() {
        bail!(
            "Output directory '{}' already exists. Please remove it or choose a different directory to prevent data loss.",
            output_dir.display()
        );
    }

    println!("Scanning for .note files in '{}'...", input_dir.display());
    let jobs: Vec<(PathBuf, PathBuf)> = WalkDir::new(input_dir)
        .into_iter()
        .filter_map(Result::ok) // Ignore errors during walk
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|s| s == "note"))
        .map(|entry| {
            let input_path = entry.into_path();
            // Create the corresponding output path by mirroring the directory structure
            let relative_path = input_path.strip_prefix(input_dir).expect("Path from WalkDir should have a known prefix");
            let mut output_path = output_dir.join(relative_path);
            output_path.set_extension(if extract_text {
                "txt"
            } else if markdown_only {
                "md"
            } else {
                "pdf"
            });
            (input_path, output_path)
        })
        .collect();

    if jobs.is_empty() {
        println!("No .note files found. Exiting.");
        return Ok(());
    }

    let num_jobs = jobs.len();
    if extract_text {
        println!("Found {} files to extract text from. Starting extraction...", num_jobs);
    } else if pdf_and_markdown {
        println!("Found {} files to convert and generate markdown for. Starting processing...", num_jobs);
    } else if markdown_only {
        println!("Found {} files to generate markdown for. Starting processing...", num_jobs);
    } else {
        println!("Found {} files to convert. Starting conversion...", num_jobs);
    }
    let start = Instant::now();

    let pb = ProgressBar::new(num_jobs as u64);
    jobs.into_par_iter().for_each(|(input_path, output_path)| {
        let file_name = input_path.file_name().unwrap_or_default().to_string_lossy();
        let action = if extract_text {
            "Extracting text from"
        } else if pdf_and_markdown {
            "Converting and extracting text from"
        } else if markdown_only {
            "Extracting markdown from"
        } else {
            "Converting"
        };
        pb.set_message(format!("{action} {}...", file_name));
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create output subdirectory");
        }

        let result = if extract_text {
            extract_note_text(&input_path, &output_path, normalize_text_whitespace)
        } else if pdf_and_markdown {
            convert_note_to_pdf(&input_path, &output_path).and_then(|_| {
                let markdown_path = markdown_output_for_pdf_path(&output_path);
                extract_note_markdown(&input_path, &markdown_path, Some(&output_path), normalize_text_whitespace)
            })
        } else if markdown_only {
            extract_note_markdown(&input_path, &output_path, None, normalize_text_whitespace)
        } else {
            convert_note_to_pdf(&input_path, &output_path)
        };
        if let Err(e) = result {
            pb.println(format!("Failed to process '{}': {}", input_path.display(), e));
        }
        pb.inc(1);
    });

    let done_message = if extract_text {
        "All text files extracted!"
    } else if pdf_and_markdown {
        "All PDF and markdown files generated!"
    } else if markdown_only {
        "All markdown files generated!"
    } else {
        "All files converted!"
    };
    pb.finish_with_message(done_message);
    if extract_text {
        println!("Extracted text from {} files in {:?}", num_jobs, start.elapsed());
    } else if pdf_and_markdown {
        println!("Converted and generated markdown for {} files in {:?}", num_jobs, start.elapsed());
    } else if markdown_only {
        println!("Generated markdown for {} files in {:?}", num_jobs, start.elapsed());
    } else {
        println!("Converted {} files in {:?}", num_jobs, start.elapsed());
    }

    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.normalize_text_whitespace && !(cli.pdf_and_markdown || cli.markdown_only || cli.extract_text) {
        bail!("--normalize-text-whitespace requires --pdf-and-markdown, --markdown-only, or --extract-text");
    }

    if !cli.input.exists() {
        bail!("Input path '{}' does not exist.", cli.input.display());
    }

    if cli.input.is_dir() {
        process_directory(
            &cli.input,
            &cli.output,
            cli.extract_text,
            cli.pdf_and_markdown,
            cli.markdown_only,
            cli.normalize_text_whitespace,
        )?;
    } else if cli.input.is_file() {
        process_single_file(
            &cli.input,
            &cli.output,
            cli.extract_text,
            cli.pdf_and_markdown,
            cli.markdown_only,
            cli.normalize_text_whitespace,
        )?;
    } else {
        bail!("Input path '{}' is not a regular file or directory.", cli.input.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Notebook, Page, clean_duplicate_recognized_text, decode_base64, markdown_output_for_pdf_path, normalize_text_whitespace,
        notebook_to_markdown, parse_recognition_payload,
    };
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn parse_recognition_payload_extracts_text_elements() {
        let payload = "eyJlbGVtZW50cyI6W3sidHlwZSI6IlRleHQiLCJsYWJlbCI6IkxpbmUgb25lIn0seyJ0eXBlIjoiU2hhcGUiLCJsYWJlbCI6Imlnbm9yZSBtZSJ9LHsidHlwZSI6IlRleHQiLCJsYWJlbCI6IkxpbmUgdHdvIn1dfQ==";
        let parsed = parse_recognition_payload(payload).expect("payload should parse");
        assert_eq!(parsed.as_deref(), Some("Line one\nLine two"));
    }

    #[test]
    fn parse_recognition_payload_falls_back_to_any_labels() {
        let payload = "eyJlbGVtZW50cyI6W3sidHlwZSI6IlNoYXBlIiwibGFiZWwiOiJ4In1dfQ==";
        let parsed = parse_recognition_payload(payload).expect("payload should parse");
        assert_eq!(parsed.as_deref(), Some("x"));
        assert_eq!(parse_recognition_payload("   ").expect("blank should parse"), None);
    }

    #[test]
    fn parse_recognition_payload_dedupes_punctuation_spaced_variants() {
        let payload =
            "eyJlbGVtZW50cyI6W3sidHlwZSI6IlRleHQiLCJsYWJlbCI6IkhlbGxvLCB3b3JsZC4ifSx7InR5cGUiOiJUZXh0IiwibGFiZWwiOiJIZWxsbyAsIHdvcmxkIC4ifV19";
        let parsed = parse_recognition_payload(payload).expect("payload should parse");
        assert_eq!(parsed.as_deref(), Some("Hello, world."));
    }

    #[test]
    fn parse_recognition_payload_dedupes_near_duplicate_labels() {
        let payload = "eyJlbGVtZW50cyI6W3sidHlwZSI6IlRleHQiLCJsYWJlbCI6IlByYXllciBsaXN0LiBNYXR0aGV3IDIxOjIyIEFuZCBhbGwgdGhpbmdzLCB3aGF0c29ldmVyIHllIHNoYWxsIGFzayBpbiBwcmF5ZXIsIGJlbGlldmluZywgeWUgc2hhbGwgcmVjZWl2ZS4ifSx7InR5cGUiOiJUZXh0IiwibGFiZWwiOiJQcmF5ZXIgbGlzdCAuIE1hdHRoZXcgMjE6MjIgQW5kIGFsbCB0aGluZ3MgLCB3aGF0c29ldmVyIHllIHNoYWxsIGFzayBpbiBiZWxpZXZpbmcgcmVjZWl2ZSJ9XX0=";
        let parsed = parse_recognition_payload(payload).expect("payload should parse");
        assert_eq!(
            parsed.as_deref(),
            Some("Prayer list. Matthew 21:22 And all things, whatsoever ye shall ask in prayer, believing, ye shall receive.")
        );
    }

    #[test]
    fn decode_base64_supports_urlsafe_without_padding() {
        let decoded = decode_base64("SGVsbG8td29ybGQ").expect("urlsafe should decode");
        assert_eq!(decoded, b"Hello-world");
    }

    #[test]
    fn notebook_to_markdown_includes_title_and_page_sections() {
        let notebook = Notebook {
            signature: "SN_FILE_VER_20230015".to_string(),
            pages: vec![
                Page {
                    addr: 1,
                    layers: vec![],
                    recognized_text: Some("Line one\nLine two".to_string()),
                },
                Page {
                    addr: 2,
                    layers: vec![],
                    recognized_text: None,
                },
            ],
            width: 1404,
            height: 1872,
            metadata: HashMap::new(),
        };

        let markdown = notebook_to_markdown(
            Path::new("My Notes/Meeting Agenda.note"),
            Some(Path::new("Archive/Meeting Agenda.pdf")),
            &notebook,
            false,
        );
        assert!(markdown.contains("# Meeting Agenda"));
        assert!(markdown.contains("## PDF Attachment"));
        assert!(markdown.contains("## Text"));
    }

    #[test]
    fn markdown_output_for_pdf_path_replaces_extension() {
        let markdown_path = markdown_output_for_pdf_path(Path::new("output/subdir/note.pdf"));
        assert_eq!(markdown_path, Path::new("output/subdir/note.md"));
    }

    #[test]
    fn notebook_to_markdown_omits_pdf_section_when_no_pdf_output() {
        let notebook = Notebook {
            signature: "SN_FILE_VER_20230015".to_string(),
            pages: vec![Page {
                addr: 1,
                layers: vec![],
                recognized_text: Some("Line one".to_string()),
            }],
            width: 1404,
            height: 1872,
            metadata: HashMap::new(),
        };

        let markdown = notebook_to_markdown(Path::new("My Notes/Meeting Agenda.note"), None, &notebook, false);
        assert!(!markdown.contains("## PDF Attachment"));
        assert!(!markdown.contains("pdf_attachment:"));
        assert!(markdown.contains("## Text"));
    }

    #[test]
    fn normalize_text_whitespace_preserves_paragraphs() {
        let input = "Line one\nline two\n\nPara two\nline b";
        let normalized = normalize_text_whitespace(input);
        assert_eq!(normalized, "Line one line two\n\nPara two line b");
    }

    #[test]
    fn clean_duplicate_recognized_text_removes_word_per_line_duplicate() {
        let input = "hello world from note\n\nhello\nworld\nfrom\nnote";
        let cleaned = clean_duplicate_recognized_text(input);
        assert_eq!(cleaned, "hello world from note");
    }

    #[test]
    fn trim_inline_near_duplicate_passage_removes_repeated_tail() {
        let base = "Prayer list Matthew 21 22 and all things whatsoever ye shall ask in prayer believing ye shall receive sunday singing and church notes and family names";
        let duplicate =
            "Prayer list Matthew 21 22 and all things whatsoever ye shall ask in believing receive sunday singing and church notes and family names";
        let input = format!("{} {}", base, duplicate);
        let trimmed = super::trim_inline_near_duplicate_passage(&input);
        assert_eq!(trimmed, base);
    }
}
