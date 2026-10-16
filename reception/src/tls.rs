//! Certificat de l'appareil : auto-signé, créé une fois et gardé dans
//! ~/.local/share/odysseus/reception. LocalSend chiffre ainsi les échanges sans
//! autorité de certification ; son empreinte (SHA-256) identifie l'appareil.

use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf, sync::Arc};

use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use sha2::{Digest, Sha256};

pub struct Tls {
    pub config: Arc<ServerConfig>,
    pub fingerprint: String,
}

fn dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME inconnu")?;
    Ok(PathBuf::from(home).join(".local/share/odysseus/reception"))
}

pub fn load_or_create() -> Result<Tls, String> {
    let dir = dir()?;
    let (cert_path, key_path) = (dir.join("certificat.der"), dir.join("cle.der"));
    let (cert, key) = match (fs::read(&cert_path), fs::read(&key_path)) {
        (Ok(c), Ok(k)) => (c, k),
        _ => {
            let made = rcgen::generate_simple_self_signed(vec!["odysseus".to_string()])
                .map_err(|e| e.to_string())?;
            let (cert, key) = (made.cert.der().to_vec(), made.key_pair.serialize_der());
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            fs::write(&cert_path, &cert).map_err(|e| e.to_string())?;
            // Clé privée lisible par l'utilisateur seul
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&key_path)
                .and_then(|mut f| f.write_all(&key))
                .map_err(|e| e.to_string())?;
            (cert, key)
        }
    };
    let fingerprint = Sha256::digest(&cert)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .map_err(|e| e.to_string())?;
    Ok(Tls {
        config: Arc::new(config),
        fingerprint,
    })
}
