//! Reading binaries out of a release archive (`tar.gz` with the binaries at
//! the top level, as release.yml packages them).

use std::collections::HashMap;
use std::io::Read;

/// Largest binary accepted from an archive.
pub const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

/// Extracts the regular files named in `names` from the top level of a
/// `tar.gz` archive. Links, directories and nested paths are ignored, so an
/// archive can't make the updater write anywhere but where it chooses.
pub fn extract(archive: &[u8], names: &[&str]) -> anyhow::Result<HashMap<String, Vec<u8>>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut found = HashMap::new();
    for entry in tar
        .entries()
        .map_err(|e| anyhow::anyhow!("Not a tar.gz archive: {}", e))?
    {
        let mut entry = entry.map_err(|e| anyhow::anyhow!("Corrupt archive: {}", e))?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = entry.path()?.into_owned();
        let parts: Vec<_> = path
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect();
        let [std::path::Component::Normal(name)] = parts.as_slice() else {
            continue;
        };
        let Some(name) = name.to_str().filter(|n| names.contains(n)) else {
            continue;
        };
        if entry.size() > MAX_BINARY_BYTES {
            anyhow::bail!("{} in the archive is too large", name);
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        (&mut entry)
            .take(MAX_BINARY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| anyhow::anyhow!("Corrupt archive: {}", e))?;
        found.insert(name.to_string(), bytes);
    }
    Ok(found)
}

/// Builds a `tar.gz` with `files` at the top level (for tests and fixtures).
pub fn build_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, *bytes).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_top_level_binaries_only() {
        let archive = build_tar_gz(&[
            ("aegis-daemon", b"daemon"),
            ("./aegis-cli", b"cli"),
            ("nested/aegis-tui", b"nested"),
            ("README", b"readme"),
        ]);
        let found = extract(&archive, &["aegis-daemon", "aegis-cli", "aegis-tui"]).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found["aegis-daemon"], b"daemon");
        assert_eq!(found["aegis-cli"], b"cli");
        assert!(extract(b"not gzip", &["aegis-daemon"]).is_err());
    }
}
