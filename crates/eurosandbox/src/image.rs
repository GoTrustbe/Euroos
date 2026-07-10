//! **Signed container image** (3F-1) — a container's identity + policy is an
//! Ed25519-signed manifest, verified *before* the container runs. This is the
//! sovereign analogue of a signed OCI image: a tampered manifest (elevated
//! capabilities, a wider net scope, a swapped root filesystem) is refused. The
//! Ed25519 verifier + SHA-256 hasher are injected (the kernel passes its
//! baked-in dev key), so the crate stays `no_std` and host-testable.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::limits::ResourceLimits;

/// The manifest that names and constrains a container image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageManifest {
    pub name: String,
    /// Permitted capability mask.
    pub caps: u64,
    pub limits: ResourceLimits,
    /// Allowed network host:port pairs (empty = no network).
    pub net: Vec<(String, u16)>,
    /// SHA-256 (hex) of the read-only root filesystem image.
    pub rootfs_sha256: String,
}

impl ImageManifest {
    /// Canonical, deterministic serialization — the exact bytes that are signed.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&alloc::format!("name={}\n", self.name));
        s.push_str(&alloc::format!("caps={}\n", self.caps));
        s.push_str(&alloc::format!(
            "limits={},{},{},{}\n",
            self.limits.max_mem_bytes, self.limits.max_pids, self.limits.max_cpu_ms, self.limits.max_wall_ms
        ));
        // Net entries in a stable order.
        let mut nets: Vec<String> = self.net.iter().map(|(h, p)| alloc::format!("{h}:{p}")).collect();
        nets.sort();
        s.push_str(&alloc::format!("net={}\n", nets.join(",")));
        s.push_str(&alloc::format!("rootfs={}\n", self.rootfs_sha256));
        s.into_bytes()
    }

    /// Parse the canonical form back (for on-disk images).
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let text = core::str::from_utf8(data).ok()?;
        let mut name = None;
        let mut caps = None;
        let mut limits = ResourceLimits::default();
        let mut net = Vec::new();
        let mut rootfs = None;
        for line in text.lines() {
            let (k, v) = line.split_once('=')?;
            match k {
                "name" => name = Some(v.to_string()),
                "caps" => caps = v.parse().ok(),
                "limits" => {
                    let p: Vec<u64> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                    if p.len() == 4 {
                        limits = ResourceLimits::new(p[0], p[1] as u32, p[2], p[3]);
                    }
                }
                "net" => {
                    for pair in v.split(',').filter(|s| !s.is_empty()) {
                        if let Some((h, port)) = pair.rsplit_once(':') {
                            if let Ok(pn) = port.parse() {
                                net.push((h.to_string(), pn));
                            }
                        }
                    }
                }
                "rootfs" => rootfs = Some(v.to_string()),
                _ => {}
            }
        }
        Some(Self { name: name?, caps: caps?, limits, net, rootfs_sha256: rootfs? })
    }
}

/// A signed image = manifest bytes + an Ed25519 signature over them.
#[derive(Debug, Clone)]
pub struct SignedImage {
    pub manifest: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImageError {
    BadSignature,
    BadManifest,
    /// The provided root filesystem does not match the signed hash.
    RootfsMismatch,
}

/// Verify a signed image against the OS key (`verify`) and, if `rootfs_hash` is
/// supplied, that the actual root filesystem matches the signed hash. Returns
/// the trusted manifest. A tampered manifest, a forged signature, or a swapped
/// rootfs is refused — the container never starts.
pub fn verify_image(
    image: &SignedImage,
    verify: &dyn Fn(&[u8], &[u8]) -> bool,
    rootfs_hash_hex: Option<&str>,
) -> Result<ImageManifest, ImageError> {
    if !verify(&image.manifest, &image.signature) {
        return Err(ImageError::BadSignature);
    }
    let manifest = ImageManifest::from_bytes(&image.manifest).ok_or(ImageError::BadManifest)?;
    if let Some(h) = rootfs_hash_hex {
        if h != manifest.rootfs_sha256 {
            return Err(ImageError::RootfsMismatch);
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

    fn key() -> (SigningKey, VerifyingKey) {
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn verifier(vk: VerifyingKey) -> impl Fn(&[u8], &[u8]) -> bool {
        move |m: &[u8], s: &[u8]| {
            ed25519_dalek::Signature::from_slice(s).map(|sig| vk.verify(m, &sig).is_ok()).unwrap_or(false)
        }
    }

    fn sample() -> ImageManifest {
        ImageManifest {
            name: "web".to_string(),
            caps: 0b0111,
            limits: ResourceLimits::new(64 * 1024 * 1024, 16, 5000, 60000),
            net: alloc::vec![("euro-os.eu".to_string(), 443)],
            rootfs_sha256: "abc123".to_string(),
        }
    }

    #[test]
    fn manifest_roundtrips() {
        let m = sample();
        let back = ImageManifest::from_bytes(&m.to_bytes()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn valid_signature_and_rootfs_accepted() {
        let (sk, vk) = key();
        let m = sample();
        let bytes = m.to_bytes();
        let sig = sk.sign(&bytes).to_bytes().to_vec();
        let img = SignedImage { manifest: bytes, signature: sig };
        let out = verify_image(&img, &verifier(vk), Some("abc123")).unwrap();
        assert_eq!(out.name, "web");
        assert_eq!(out.caps, 0b0111);
    }

    #[test]
    fn tampered_manifest_refused() {
        let (sk, vk) = key();
        let m = sample();
        let sig = sk.sign(&m.to_bytes()).to_bytes().to_vec();
        // Attacker elevates the caps in the manifest but keeps the old signature.
        let mut evil = m.clone();
        evil.caps = u64::MAX;
        let img = SignedImage { manifest: evil.to_bytes(), signature: sig };
        assert_eq!(verify_image(&img, &verifier(vk), None).unwrap_err(), ImageError::BadSignature);
    }

    #[test]
    fn swapped_rootfs_refused() {
        let (sk, vk) = key();
        let m = sample();
        let bytes = m.to_bytes();
        let sig = sk.sign(&bytes).to_bytes().to_vec();
        let img = SignedImage { manifest: bytes, signature: sig };
        // Signature is valid, but the actual rootfs hash differs from the signed one.
        assert_eq!(
            verify_image(&img, &verifier(vk), Some("deadbeef")).unwrap_err(),
            ImageError::RootfsMismatch
        );
    }
}
