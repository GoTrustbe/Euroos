//! eupkg — EuroKernel Package Manager (Track 6, layer 4).
//!
//! Builds and verifies `.eupkg` packages: a ZIP with MANIFEST.toml, the binary,
//! a SHA256 hash of the binary in the manifest, and an Ed25519 signature
//! over the manifest. Privacy-by-design: no telemetry, reproducible.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

#[derive(Parser)]
#[command(name = "eupkg", version, about = "EuroKernel Package Manager")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate an Ed25519 developer key pair (key/pub).
    Keygen {
        #[arg(default_value = "keys/dev")]
        out: String,
    },
    /// Build a .eupkg from a directory with MANIFEST.toml + the binary.
    Build {
        dir: String,
        #[arg(long, default_value = "keys/dev.key")]
        key: String,
    },
    /// Verify signature + binary hash of a .eupkg.
    Verify {
        pkg: String,
        #[arg(long, default_value = "keys/dev.pub")]
        pubkey: String,
    },
    /// Show the contents/metadata of a .eupkg.
    Info { pkg: String },
}

#[derive(Serialize, Deserialize, Debug)]
struct Manifest {
    package: Package,
    #[serde(default)]
    build: Build,
    #[serde(default)]
    sandbox: Sandbox,
}

#[derive(Serialize, Deserialize, Debug)]
struct Package {
    name: String,
    version: String,
    description: String,
    license: String,
    binary: String, // path to the binary within the package (e.g. "bin/hello")
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Build {
    #[serde(default)]
    binary_sha256: String,
    #[serde(default)]
    reproducible: bool,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Sandbox {
    #[serde(default)]
    network: bool,
    #[serde(default)]
    filesystem: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn main() {
    if let Err(e) = run() {
        eprintln!("eupkg: error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().cmd {
        Cmd::Keygen { out } => keygen(&out),
        Cmd::Build { dir, key } => build(&dir, &key),
        Cmd::Verify { pkg, pubkey } => verify(&pkg, &pubkey),
        Cmd::Info { pkg } => info(&pkg),
    }
}

fn keygen(out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let vk = sk.verifying_key();
    if let Some(p) = Path::new(out).parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(format!("{out}.key"), sk.to_bytes())?;
    fs::write(format!("{out}.pub"), vk.to_bytes())?;
    println!("Ed25519 key pair:");
    println!("  private: {out}.key");
    println!("  public:  {out}.pub  ({})", hex::encode(vk.to_bytes()));
    Ok(())
}

fn build(dir: &str, key: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(dir);
    let manifest_path = dir.join("MANIFEST.toml");
    let mut manifest: Manifest = toml::from_str(&fs::read_to_string(&manifest_path)?)?;

    // Read and hash the binary.
    let bin_path = dir.join(&manifest.package.binary);
    let bin = fs::read(&bin_path)?;
    let hash = sha256_hex(&bin);
    manifest.build.binary_sha256 = hash.clone();
    let manifest_str = toml::to_string_pretty(&manifest)?;

    // Sign the manifest (Ed25519).
    let sk_bytes: [u8; 32] = fs::read(key)?.as_slice().try_into().map_err(|_| "invalid key")?;
    let sk = SigningKey::from_bytes(&sk_bytes);
    let sig: Signature = sk.sign(manifest_str.as_bytes());

    // Write the .eupkg (ZIP).
    let out = format!("{}-{}.eupkg", manifest.package.name, manifest.package.version);
    let mut zip = zip::ZipWriter::new(File::create(&out)?);
    let opt = SimpleFileOptions::default();
    zip.start_file("MANIFEST.toml", opt)?;
    zip.write_all(manifest_str.as_bytes())?;
    zip.start_file("signature.ed25519", opt)?;
    zip.write_all(&sig.to_bytes())?;
    zip.start_file(&manifest.package.binary, opt)?;
    zip.write_all(&bin)?;
    zip.finish()?;

    println!("built: {out}");
    println!("  {} v{}", manifest.package.name, manifest.package.version);
    println!("  binary: {} ({} bytes)", manifest.package.binary, bin.len());
    println!("  sha256: {hash}");
    println!("  signed with Ed25519");
    Ok(())
}

fn read_zip_file(pkg: &str, name: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut zip = zip::ZipArchive::new(File::open(pkg)?)?;
    let mut f = zip.by_name(name)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn verify(pkg: &str, pubkey: &str) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_bytes = read_zip_file(pkg, "MANIFEST.toml")?;
    let sig_bytes = read_zip_file(pkg, "signature.ed25519")?;
    let manifest: Manifest = toml::from_str(std::str::from_utf8(&manifest_bytes)?)?;

    // Verify the signature.
    let vk_bytes: [u8; 32] = fs::read(pubkey)?.as_slice().try_into().map_err(|_| "invalid pubkey")?;
    let vk = VerifyingKey::from_bytes(&vk_bytes)?;
    let sig = Signature::from_bytes(sig_bytes.as_slice().try_into().map_err(|_| "invalid sig")?);
    vk.verify(&manifest_bytes, &sig)?;
    println!("[OK] Ed25519 signature valid");

    // Verify the binary hash.
    let bin = read_zip_file(pkg, &manifest.package.binary)?;
    let hash = sha256_hex(&bin);
    if hash == manifest.build.binary_sha256 {
        println!("[OK] binary SHA256 matches ({hash})");
    } else {
        return Err("binary hash DOES NOT match — package tampered with!".into());
    }
    println!(
        "[OK] {} v{} verified, ready to install",
        manifest.package.name, manifest.package.version
    );
    Ok(())
}

fn info(pkg: &str) -> Result<(), Box<dyn std::error::Error>> {
    let manifest: Manifest = toml::from_str(std::str::from_utf8(&read_zip_file(pkg, "MANIFEST.toml")?)?)?;
    println!("{} v{}", manifest.package.name, manifest.package.version);
    println!("  {}", manifest.package.description);
    println!("  license:   {}", manifest.package.license);
    println!("  binary:    {}", manifest.package.binary);
    println!("  sha256:    {}", manifest.build.binary_sha256);
    println!("  sandbox:   network={}  fs={}", manifest.sandbox.network, manifest.sandbox.filesystem);
    Ok(())
}
