//! Post-build setup for users who compile Nellie from source.
//!
//! Downloads the embedding model, tokenizer, and ONNX Runtime to the data
//! directory. Reuses the same URLs and SHA-256 checksums as
//! `packaging/install-universal.sh` so both install paths produce identical
//! artefacts.
//!
//! # Usage
//!
//! ```bash
//! nellie setup                       # downloads everything
//! nellie setup --skip-runtime        # skip ONNX Runtime
//! nellie setup --skip-model          # skip model + tokenizer
//! nellie setup --data-dir /custom    # custom data directory
//! ```

use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;
use tokio::io::AsyncWriteExt;

// ---------------------------------------------------------------------------
// Constants — must match packaging/install-universal.sh
// ---------------------------------------------------------------------------

/// ONNX Runtime version pinned by the install script.
const ORT_VERSION: &str = "1.24.4";

/// SHA-256 checksums for ONNX Runtime archives, keyed by platform.
const ORT_SHA256_LINUX_X64: &str =
    "3a211fbea252c1e66290658f1b735b772056149f28321e71c308942cdb54b747";
const ORT_SHA256_LINUX_ARM64: &str =
    "866109a9248d057671a039b9d725be4bd86888e3754140e6701ec621be9d4d7e";
const ORT_SHA256_MACOS_ARM64: &str =
    "93787795f47e1eee369182e43ed51b9e5da0878ab0346aecf4258979b8bba989";

/// Embedding model (all-MiniLM-L6-v2) URL and checksum.
const MODEL_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
const MODEL_SHA256: &str = "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452";

/// Tokenizer URL and checksum.
const TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";
const TOKENIZER_SHA256: &str = "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during the setup process.
#[derive(Debug, Error)]
pub enum SetupError {
    /// An HTTP request failed.
    #[error("download failed for {url}: {source}")]
    Download { url: String, source: reqwest::Error },

    /// SHA-256 checksum mismatch after download.
    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    Checksum {
        path: String,
        expected: String,
        actual: String,
    },

    /// The current platform is not supported.
    #[error("unsupported platform: {os}-{arch}")]
    UnsupportedPlatform { os: String, arch: String },

    /// A filesystem operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Archive extraction failed.
    #[error("failed to extract archive: {0}")]
    Extract(String),
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Supported platform targets for ONNX Runtime downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux on x86_64
    LinuxX64,
    /// Linux on aarch64
    LinuxArm64,
    /// macOS on Apple Silicon (arm64)
    MacosArm64,
}

impl Platform {
    /// Detect the current platform from `std::env::consts`.
    ///
    /// # Errors
    ///
    /// Returns [`SetupError::UnsupportedPlatform`] if the OS/arch combination
    /// is not supported.
    pub fn detect() -> std::result::Result<Self, SetupError> {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        match (os, arch) {
            ("linux", "x86_64") => Ok(Self::LinuxX64),
            ("linux", "aarch64") => Ok(Self::LinuxArm64),
            ("macos", "aarch64") => Ok(Self::MacosArm64),
            _ => Err(SetupError::UnsupportedPlatform {
                os: os.to_string(),
                arch: arch.to_string(),
            }),
        }
    }

    /// Returns the ONNX Runtime archive filename for this platform.
    fn ort_archive_name(self) -> String {
        match self {
            Self::LinuxX64 => format!("onnxruntime-linux-x64-{ORT_VERSION}.tgz"),
            Self::LinuxArm64 => format!("onnxruntime-linux-aarch64-{ORT_VERSION}.tgz"),
            Self::MacosArm64 => format!("onnxruntime-osx-arm64-{ORT_VERSION}.tgz"),
        }
    }

    /// Returns the expected SHA-256 checksum for this platform's ORT archive.
    fn ort_sha256(self) -> &'static str {
        match self {
            Self::LinuxX64 => ORT_SHA256_LINUX_X64,
            Self::LinuxArm64 => ORT_SHA256_LINUX_ARM64,
            Self::MacosArm64 => ORT_SHA256_MACOS_ARM64,
        }
    }

    /// Returns the shared library filename for this platform.
    fn lib_filename(self) -> &'static str {
        match self {
            Self::LinuxX64 | Self::LinuxArm64 => "libonnxruntime.so",
            Self::MacosArm64 => "libonnxruntime.dylib",
        }
    }

    /// Returns the glob pattern matching shared library files inside the
    /// extracted ORT archive (covers versioned symlinks like `libonnxruntime.so.1.24.4`).
    fn lib_glob_prefix(self) -> &'static str {
        match self {
            Self::LinuxX64 | Self::LinuxArm64 => "libonnxruntime.so",
            Self::MacosArm64 => "libonnxruntime",
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LinuxX64 => write!(f, "linux-x86_64"),
            Self::LinuxArm64 => write!(f, "linux-aarch64"),
            Self::MacosArm64 => write!(f, "macos-arm64"),
        }
    }
}

// ---------------------------------------------------------------------------
// Core download + verify helpers
// ---------------------------------------------------------------------------

/// Download a URL to a local file path, computing SHA-256 on the fly.
///
/// Returns the hex-encoded SHA-256 digest of the downloaded bytes.
///
/// # Errors
///
/// Returns [`SetupError::Download`] on HTTP errors and [`SetupError::Io`] on
/// filesystem errors.
async fn download_to_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> std::result::Result<String, SetupError> {
    use futures::StreamExt;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| SetupError::Download {
            url: url.to_string(),
            source: e,
        })?;

    let status = response.status();
    if !status.is_success() {
        return Err(SetupError::Download {
            url: url.to_string(),
            source: response.error_for_status().unwrap_err(),
        });
    }

    let total_bytes = response.content_length();
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(dest).await?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| SetupError::Download {
            url: url.to_string(),
            source: e,
        })?;
        file.write_all(&chunk).await?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;

        if let Some(total) = total_bytes {
            let pct = (downloaded * 100) / total;
            eprint!("\r  downloading... {downloaded}/{total} bytes ({pct}%)");
        } else {
            eprint!("\r  downloading... {downloaded} bytes");
        }
    }
    eprintln!();
    file.flush().await?;
    file.sync_all().await?;

    let digest = hex::encode(hasher.finalize());
    Ok(digest)
}

/// Verify that `actual` matches `expected` SHA-256 hex digest.
///
/// # Errors
///
/// Returns [`SetupError::Checksum`] on mismatch.
fn verify_checksum(
    path: &Path,
    expected: &str,
    actual: &str,
) -> std::result::Result<(), SetupError> {
    if actual != expected {
        return Err(SetupError::Checksum {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Individual download tasks
// ---------------------------------------------------------------------------

/// Download and install the ONNX Runtime shared library.
///
/// The archive is downloaded to a temporary directory, verified, extracted,
/// and the shared library files are copied into `{data_dir}/lib/`.
///
/// # Errors
///
/// Returns errors on download failure, checksum mismatch, or extraction issues.
async fn download_ort(
    client: &reqwest::Client,
    data_dir: &Path,
    platform: Platform,
) -> std::result::Result<(), SetupError> {
    let lib_dir = data_dir.join("lib");
    let lib_path = lib_dir.join(platform.lib_filename());

    if lib_path.exists() {
        tracing::info!(
            "ONNX Runtime already present at {}, skipping",
            lib_path.display()
        );
        eprintln!(
            "  [skip] ONNX Runtime already present at {}",
            lib_path.display()
        );
        return Ok(());
    }

    let archive_name = platform.ort_archive_name();
    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/v{ORT_VERSION}/{archive_name}"
    );

    tracing::info!("Downloading ONNX Runtime {ORT_VERSION} for {platform}");
    eprintln!("  Downloading ONNX Runtime {ORT_VERSION} ({platform})...");

    // Download to a temp file
    let tmp_dir = tempfile::tempdir()?;
    let archive_path = tmp_dir.path().join(&archive_name);
    let digest = download_to_file(client, &url, &archive_path).await?;
    verify_checksum(&archive_path, platform.ort_sha256(), &digest)?;
    tracing::info!("SHA-256 verified for {archive_name}");
    eprintln!("  SHA-256 verified");

    // Extract the archive
    tokio::fs::create_dir_all(&lib_dir).await?;
    extract_ort_archive(&archive_path, &lib_dir, platform).await?;

    tracing::info!("ONNX Runtime installed to {}", lib_dir.display());
    eprintln!("  [ok] ONNX Runtime installed to {}", lib_dir.display());
    Ok(())
}

/// Extract the ORT archive and copy shared library files to `lib_dir`.
async fn extract_ort_archive(
    archive_path: &Path,
    lib_dir: &Path,
    platform: Platform,
) -> std::result::Result<(), SetupError> {
    let archive_path = archive_path.to_path_buf();
    let lib_dir = lib_dir.to_path_buf();

    // Archive extraction is CPU-bound / blocking I/O, run on blocking pool
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive_path).map_err(SetupError::Io)?;
        let gz = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(gz);

        let lib_prefix = platform.lib_glob_prefix();

        for entry_result in archive
            .entries()
            .map_err(|e| SetupError::Extract(e.to_string()))?
        {
            let mut entry = entry_result.map_err(|e| SetupError::Extract(e.to_string()))?;
            let path = entry
                .path()
                .map_err(|e| SetupError::Extract(e.to_string()))?
                .to_path_buf();

            // We want files in the lib/ subdirectory whose name starts with
            // the platform library prefix (e.g., libonnxruntime.so*)
            if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                if fname.starts_with(lib_prefix) {
                    let dest = lib_dir.join(fname);
                    // Tar entries can be symlinks (e.g., libonnxruntime.so -> libonnxruntime.so.1).
                    // Symlink entries have 0 bytes of data, so io::copy produces empty files.
                    // Extract only regular files; we create symlinks after the loop.
                    if entry.header().entry_type() == tar::EntryType::Regular {
                        let mut out = std::fs::File::create(&dest).map_err(SetupError::Io)?;
                        std::io::copy(&mut entry, &mut out)
                            .map_err(|e| SetupError::Extract(e.to_string()))?;
                        tracing::debug!("Extracted {fname}");
                    }
                }
            }
        }

        // Create symlinks for the versioned library.
        // The tarball contains e.g. libonnxruntime.so.1.24.4 (real file)
        // plus symlinks libonnxruntime.so.1 and libonnxruntime.so.
        // Since we skipped symlink entries above, recreate them here.
        let base = platform.lib_filename(); // e.g. "libonnxruntime.so"
        let mut versioned: Option<String> = None;
        for f in std::fs::read_dir(&lib_dir)
            .map_err(SetupError::Io)?
            .flatten()
        {
            let name = f.file_name().to_string_lossy().to_string();
            if name.starts_with(base) && name != base && f.metadata().is_ok_and(|m| m.len() > 0) {
                // Pick the longest filename (e.g. libonnxruntime.so.1.24.4
                // over libonnxruntime.so.1) to ensure we symlink to the
                // fully-versioned real file, not a shorter intermediate.
                if versioned
                    .as_ref()
                    .map_or(true, |prev| name.len() > prev.len())
                {
                    versioned = Some(name);
                }
            }
        }
        if let Some(ref real_name) = versioned {
            let real_path = lib_dir.join(real_name);
            let link_path = lib_dir.join(base);
            if link_path.exists() || link_path.symlink_metadata().is_ok() {
                std::fs::remove_file(&link_path).ok();
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&real_path, &link_path)
                .map_err(|e| SetupError::Extract(format!("symlink {base}: {e}")))?;
            tracing::debug!("Symlinked {base} -> {real_name}");
        }

        Ok(())
    })
    .await
    .map_err(|e| SetupError::Extract(format!("join error: {e}")))?
}

/// Download the embedding model (all-MiniLM-L6-v2 ONNX).
///
/// # Errors
///
/// Returns errors on download failure or checksum mismatch.
async fn download_model(
    client: &reqwest::Client,
    data_dir: &Path,
) -> std::result::Result<(), SetupError> {
    let models_dir = data_dir.join("models");
    let model_path = models_dir.join("all-MiniLM-L6-v2.onnx");

    if model_path.exists() {
        tracing::info!(
            "Embedding model already present at {}, skipping",
            model_path.display()
        );
        eprintln!(
            "  [skip] Embedding model already present at {}",
            model_path.display()
        );
        return Ok(());
    }

    tokio::fs::create_dir_all(&models_dir).await?;

    tracing::info!("Downloading embedding model (all-MiniLM-L6-v2, ~87 MB)");
    eprintln!("  Downloading embedding model (all-MiniLM-L6-v2, ~87 MB)...");

    let digest = download_to_file(client, MODEL_URL, &model_path).await?;
    verify_checksum(&model_path, MODEL_SHA256, &digest)?;
    tracing::info!("SHA-256 verified for model");
    eprintln!("  SHA-256 verified");

    eprintln!(
        "  [ok] Embedding model installed to {}",
        model_path.display()
    );
    Ok(())
}

/// Download the tokenizer (tokenizer.json).
///
/// # Errors
///
/// Returns errors on download failure or checksum mismatch.
async fn download_tokenizer(
    client: &reqwest::Client,
    data_dir: &Path,
) -> std::result::Result<(), SetupError> {
    let models_dir = data_dir.join("models");
    let tokenizer_path = models_dir.join("tokenizer.json");

    if tokenizer_path.exists() {
        tracing::info!(
            "Tokenizer already present at {}, skipping",
            tokenizer_path.display()
        );
        eprintln!(
            "  [skip] Tokenizer already present at {}",
            tokenizer_path.display()
        );
        return Ok(());
    }

    tokio::fs::create_dir_all(&models_dir).await?;

    tracing::info!("Downloading tokenizer");
    eprintln!("  Downloading tokenizer...");

    let digest = download_to_file(client, TOKENIZER_URL, &tokenizer_path).await?;
    verify_checksum(&tokenizer_path, TOKENIZER_SHA256, &digest)?;
    tracing::info!("SHA-256 verified for tokenizer");
    eprintln!("  SHA-256 verified");

    eprintln!("  [ok] Tokenizer installed to {}", tokenizer_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the full setup process: detect platform, download ORT + model +
/// tokenizer, verify checksums, and print the required environment variable.
///
/// # Arguments
///
/// * `data_dir` - Base data directory (e.g. `~/.local/share/nellie`).
/// * `skip_runtime` - If `true`, skip the ONNX Runtime download.
/// * `skip_model` - If `true`, skip the model and tokenizer downloads.
///
/// # Errors
///
/// Returns [`SetupError`] on download, checksum, or I/O failures.
pub async fn run_setup(
    data_dir: &Path,
    skip_runtime: bool,
    skip_model: bool,
) -> std::result::Result<(), SetupError> {
    eprintln!();
    eprintln!("Nellie Setup");
    eprintln!("============");
    eprintln!("Data directory: {}", data_dir.display());
    eprintln!();

    let platform = Platform::detect()?;
    tracing::info!("Detected platform: {platform}");
    eprintln!("Platform: {platform}");
    eprintln!();

    let client = reqwest::Client::builder()
        .user_agent(format!("nellie/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| SetupError::Download {
            url: "(client init)".to_string(),
            source: e,
        })?;

    // --- ONNX Runtime ---
    if skip_runtime {
        eprintln!("  [skip] ONNX Runtime (--skip-runtime)");
    } else {
        download_ort(&client, data_dir, platform).await?;
    }

    // --- Embedding model ---
    if skip_model {
        eprintln!("  [skip] Embedding model (--skip-model)");
        eprintln!("  [skip] Tokenizer (--skip-model)");
    } else {
        download_model(&client, data_dir).await?;
        download_tokenizer(&client, data_dir).await?;
    }

    // --- Summary ---
    let lib_dir = data_dir.join("lib");
    let lib_path = lib_dir.join(platform.lib_filename());

    eprintln!();
    eprintln!("Setup complete.");
    eprintln!();
    eprintln!("Add this to your shell profile:");
    eprintln!();
    eprintln!("  export ORT_DYLIB_PATH=\"{}\"", lib_path.display());
    eprintln!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Hex encoding (small helper to avoid pulling in the `hex` crate)
// ---------------------------------------------------------------------------

mod hex {
    use std::fmt::Write;

    /// Encode bytes as a lowercase hex string.
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_display() {
        assert_eq!(Platform::LinuxX64.to_string(), "linux-x86_64");
        assert_eq!(Platform::LinuxArm64.to_string(), "linux-aarch64");
        assert_eq!(Platform::MacosArm64.to_string(), "macos-arm64");
    }

    #[test]
    fn test_platform_detect_returns_supported() {
        // We're running this test, so our platform must be supported
        // (CI or dev machine). If this fails, we have a gap.
        let result = Platform::detect();
        assert!(
            result.is_ok(),
            "Current platform should be supported: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_ort_archive_name_format() {
        let name = Platform::LinuxX64.ort_archive_name();
        assert!(name.starts_with("onnxruntime-linux-x64-"));
        assert!(name.ends_with(".tgz"));
        assert!(name.contains(ORT_VERSION));
    }

    #[test]
    fn test_ort_archive_name_all_platforms() {
        let linux_x64 = Platform::LinuxX64.ort_archive_name();
        let linux_arm = Platform::LinuxArm64.ort_archive_name();
        let macos = Platform::MacosArm64.ort_archive_name();

        assert_ne!(linux_x64, linux_arm);
        assert_ne!(linux_x64, macos);
        assert_ne!(linux_arm, macos);
    }

    #[test]
    fn test_lib_filename_linux() {
        assert_eq!(Platform::LinuxX64.lib_filename(), "libonnxruntime.so");
        assert_eq!(Platform::LinuxArm64.lib_filename(), "libonnxruntime.so");
    }

    #[test]
    fn test_lib_filename_macos() {
        assert_eq!(Platform::MacosArm64.lib_filename(), "libonnxruntime.dylib");
    }

    #[test]
    fn test_checksum_verification_pass() {
        let path = std::path::Path::new("/tmp/test.bin");
        let hash = "abc123";
        assert!(verify_checksum(path, hash, hash).is_ok());
    }

    #[test]
    fn test_checksum_verification_fail() {
        let path = std::path::Path::new("/tmp/test.bin");
        let err = verify_checksum(path, "expected", "actual").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected"));
        assert!(msg.contains("actual"));
        assert!(msg.contains("checksum mismatch"));
    }

    #[test]
    fn test_hex_encode() {
        assert_eq!(hex::encode([0x00]), "00");
        assert_eq!(hex::encode([0xff]), "ff");
        assert_eq!(hex::encode([0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(hex::encode([]), "");
    }

    #[test]
    fn test_sha256_constants_are_64_hex_chars() {
        // SHA-256 hex digests are exactly 64 characters
        for (name, hash) in [
            ("ORT_SHA256_LINUX_X64", ORT_SHA256_LINUX_X64),
            ("ORT_SHA256_LINUX_ARM64", ORT_SHA256_LINUX_ARM64),
            ("ORT_SHA256_MACOS_ARM64", ORT_SHA256_MACOS_ARM64),
            ("MODEL_SHA256", MODEL_SHA256),
            ("TOKENIZER_SHA256", TOKENIZER_SHA256),
        ] {
            assert_eq!(
                hash.len(),
                64,
                "{name} should be 64 hex characters, got {}",
                hash.len()
            );
            assert!(
                hash.chars().all(|c| c.is_ascii_hexdigit()),
                "{name} should contain only hex digits"
            );
        }
    }

    #[test]
    fn test_ort_sha256_per_platform() {
        // Each platform has a unique checksum
        let checksums: std::collections::HashSet<&str> = [
            Platform::LinuxX64.ort_sha256(),
            Platform::LinuxArm64.ort_sha256(),
            Platform::MacosArm64.ort_sha256(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            checksums.len(),
            3,
            "Each platform should have a unique ORT checksum"
        );
    }

    #[test]
    fn test_hex_encode_sha256_length() {
        // Verify our hex encoder produces the right length for SHA-256
        let digest = sha2::Sha256::digest(b"hello world");
        let encoded = hex::encode(digest);
        assert_eq!(encoded.len(), 64);
    }

    #[test]
    fn test_ort_version_matches_install_script() {
        // This constant must match packaging/install-universal.sh ORT_VERSION
        assert_eq!(ORT_VERSION, "1.24.4");
    }
}
