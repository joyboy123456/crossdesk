use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::{fs::File, io::BufReader};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use sha2::{Digest, Sha256};
use thiserror::Error;
use webrtc_dtls::crypto::Certificate;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Dtls(#[from] webrtc_dtls::Error),
}

pub fn generate_fingerprint(cert: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(cert);
    let bytes = hash
        .finalize()
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>();
    bytes.join(":").to_lowercase()
}

pub fn certificate_fingerprint(cert: &Certificate) -> String {
    let certificate = cert.certificate.first().expect("certificate missing");
    generate_fingerprint(certificate)
}

/// load certificate from file
pub fn load_certificate(path: &Path) -> Result<Certificate, Error> {
    let f = File::open(path)?;

    let mut reader = BufReader::new(f);
    let mut pem = String::new();
    reader.read_to_string(&mut pem)?;
    Ok(Certificate::from_pem(pem.as_str())?)
}

pub(crate) fn load_or_generate_key_and_cert(path: &Path) -> Result<Certificate, Error> {
    if path.exists() && path.is_file() {
        Ok(load_certificate(path)?)
    } else {
        generate_key_and_cert(path)
    }
}

pub(crate) fn generate_key_and_cert(path: &Path) -> Result<Certificate, Error> {
    let cert = Certificate::generate_self_signed(["ignored".to_owned()])?;
    let serialized = cert.serialize_pem();
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "certificate path has no parent directory: {}",
                path.display()
            ),
        )
        .into());
    };
    fs::create_dir_all(parent)?;
    let f = File::create(path)?;
    #[cfg(unix)]
    {
        let mut perm = f.metadata()?.permissions();
        perm.set_mode(0o400); /* r-- --- --- */
        f.set_permissions(perm)?;
    }
    let mut writer = BufWriter::new(f);
    writer.write_all(serialized.as_bytes())?;
    writer.flush()?;
    drop(writer);
    #[cfg(windows)]
    restrict_to_current_user(path);
    Ok(cert)
}

/// Restrict the private key file to the current user.
///
/// Windows has no chmod equivalent in std, so this shells out to `icacls`:
/// disable inheritance (`/inheritance:r`, which drops the inherited ACEs) and
/// grant only the current user. Best effort - a failure leaves the file
/// readable by other accounts on the machine, which is worth a warning but not
/// worth refusing to start over.
#[cfg(windows)]
fn restrict_to_current_user(path: &Path) {
    use std::process::Command;

    let Ok(user) = std::env::var("USERNAME") else {
        log::warn!("USERNAME is not set; leaving default permissions on {path:?}");
        return;
    };
    let result = Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:(R,W)"))
        .output();
    match result {
        Ok(output) if output.status.success() => {
            log::debug!("restricted {path:?} to {user}");
        }
        Ok(output) => log::warn!(
            "could not restrict permissions on {path:?}: icacls exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        ),
        Err(e) => log::warn!("could not restrict permissions on {path:?}: {e}"),
    }
}
