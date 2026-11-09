//! Stateless account fencing. Credential stores contribute metadata only.
use super::policy::Provider;
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub struct Context {
    pub executable: PathBuf,
    provider: Provider,
    config_dir: PathBuf,
}
impl Context {
    pub fn discover(provider: Provider) -> io::Result<Self> {
        let executable = crate::paths::find_on_path(provider.name())
            .into_iter()
            .next()
            .ok_or(io::ErrorKind::NotFound)?
            .canonicalize()?;
        let variable = match provider {
            Provider::Codex => "CODEX_HOME",
            Provider::Claude => "CLAUDE_CONFIG_DIR",
        };
        let config_dir = match std::env::var_os(variable) {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(std::env::var_os("HOME").ok_or(io::ErrorKind::NotFound)?).join(
                match provider {
                    Provider::Codex => ".codex",
                    Provider::Claude => ".claude",
                },
            ),
        };
        // The CLI runs outside the caller's project, so relative account roots
        // would identify a different account than the launched process uses.
        if !config_dir.is_absolute() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            executable,
            provider,
            config_dir,
        })
    }
    pub fn identity(&self) -> io::Result<String> {
        let mut digest = Sha256::new();
        digest.update(self.provider.name());
        digest.update(
            crate::paths::resolve_existing_prefix(&self.config_dir)
                .as_os_str()
                .as_encoded_bytes(),
        );
        let names = match self.provider {
            Provider::Codex => ["config.toml", "auth.json"],
            Provider::Claude => ["settings.json", ".credentials.json"],
        };
        for path in names
            .map(|name| self.config_dir.join(name))
            .iter()
            .chain(std::iter::once(&self.executable))
        {
            metadata(&mut digest, path)?;
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}
fn metadata(digest: &mut Sha256, path: &Path) -> io::Result<()> {
    let name = path.as_os_str().as_encoded_bytes();
    digest.update((name.len() as u64).to_le_bytes());
    digest.update(name);
    match fs::metadata(path) {
        Ok(m) => {
            digest.update([1]);
            for n in [
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime() as u64,
                m.mtime_nsec() as u64,
                m.ctime() as u64,
                m.ctime_nsec() as u64,
            ] {
                digest.update(n.to_le_bytes());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => digest.update([0]),
        Err(e) => return Err(e),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_files_are_stat_only_and_rotations_change_identity() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("cli");
        fs::write(&executable, "fake").unwrap();
        for provider in [Provider::Codex, Provider::Claude] {
            let context = Context {
                executable: executable.clone(),
                provider,
                config_dir: temp.path().to_owned(),
            };
            let credential = temp.path().join(match provider {
                Provider::Codex => "auth.json",
                Provider::Claude => ".credentials.json",
            });
            let before = context.identity().unwrap();
            fs::write(&credential, "DO_NOT_READ_CREDENTIAL_CONTENTS").unwrap();
            fs::set_permissions(&credential, fs::Permissions::from_mode(0o000)).unwrap();
            let after = context.identity().unwrap();
            assert_ne!(before, after);
            // Profiles may contain credentials. Their contents and metadata do
            // not participate, following the owner's explicit freshness choice.
            fs::write(temp.path().join(".claude.json"), "invalid profile").unwrap();
            assert_eq!(context.identity().unwrap(), after);
            fs::remove_file(credential).unwrap();
        }
    }
}
