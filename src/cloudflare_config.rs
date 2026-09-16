use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_yml::{Mapping, Value};

use crate::cloudflared;
use crate::config::{
    BASE_DOMAIN, CLOUDFLARE_CONFIG_FILENAME, CLOUDFLARE_DIRECTORY, CLOUDFLARED_FILENAME,
    LOOPBACK_HOST,
};
use crate::error::{FwError, Result};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallPaths {
    pub fw_executable: PathBuf,
    pub install_dir: PathBuf,
    pub cloudflare_dir: PathBuf,
    pub cloudflared: PathBuf,
    pub cloudflare_config: PathBuf,
}

impl InstallPaths {
    pub fn resolve() -> Result<Self> {
        Self::from_executable(std::env::current_exe()?)
    }

    pub fn from_executable(fw_executable: PathBuf) -> Result<Self> {
        if !fw_executable.is_absolute() {
            return Err(FwError::InvalidExecutableDirectory);
        }
        let install_dir = fw_executable
            .parent()
            .filter(|parent| parent.is_absolute())
            .ok_or(FwError::InvalidExecutableDirectory)?
            .to_path_buf();
        let cloudflare_dir = install_dir.join(CLOUDFLARE_DIRECTORY);
        let cloudflared = cloudflare_dir.join(CLOUDFLARED_FILENAME);
        let cloudflare_config = cloudflare_dir.join(CLOUDFLARE_CONFIG_FILENAME);

        require_regular_file(CLOUDFLARED_FILENAME, &cloudflared)?;
        require_regular_file(CLOUDFLARE_CONFIG_FILENAME, &cloudflare_config)?;

        Ok(Self {
            fw_executable,
            install_dir,
            cloudflare_dir,
            cloudflared,
            cloudflare_config,
        })
    }
}

/// Normalizes, validates, and atomically installs the managed Cloudflare config.
///
/// Both files in the sibling `cf` directory must already have been validated
/// through [`InstallPaths`].
/// The original config remains untouched unless the official validator accepts
/// the complete temporary candidate.
pub async fn normalize_validate_and_replace(paths: &InstallPaths, proxy_port: u16) -> Result<()> {
    // Recheck immediately before mutation in case installation files changed
    // after startup path resolution.
    require_regular_file(CLOUDFLARED_FILENAME, &paths.cloudflared)?;
    require_regular_file(CLOUDFLARE_CONFIG_FILENAME, &paths.cloudflare_config)?;

    let input = fs::read_to_string(&paths.cloudflare_config)?;
    let normalized = normalize_yaml(&input, proxy_port)?;
    let mut candidate = TempCandidate::create(&paths.cloudflare_config, normalized.as_bytes())?;

    cloudflared::validate_config(&paths.cloudflared, candidate.path(), &paths.cloudflare_dir)
        .await?;
    atomic_replace(candidate.path(), &paths.cloudflare_config)?;
    candidate.disarm();
    Ok(())
}

pub fn normalize_yaml(input: &str, proxy_port: u16) -> Result<String> {
    let document: Value = serde_yml::from_str(input)?;
    let normalized = normalize_value(document, proxy_port)?;
    Ok(serde_yml::to_string(&normalized)?)
}

pub fn normalize_value(mut document: Value, proxy_port: u16) -> Result<Value> {
    let root = document.as_mapping_mut().ok_or_else(|| {
        FwError::InvalidIngress("the YAML document must be a top-level mapping".into())
    })?;

    let had_ingress = root.contains_key("ingress");
    let entries = match root.get_mut("ingress") {
        Some(Value::Sequence(entries)) => std::mem::take(entries),
        Some(_) => {
            return Err(FwError::InvalidIngress(
                "`ingress` must be a sequence".into(),
            ));
        }
        None => Vec::new(),
    };

    let canonical_hostname = format!("*.{BASE_DOMAIN}");
    let canonical_service = format!("http://{LOOPBACK_HOST}:{proxy_port}");

    let mut specific = Vec::with_capacity(entries.len());
    let mut canonical = None;
    let mut fallback = None;

    for (index, entry) in entries.into_iter().enumerate() {
        let mut rule = match entry {
            Value::Mapping(rule) => rule,
            _ => {
                return Err(FwError::InvalidIngress(format!(
                    "ingress[{index}] must be a mapping"
                )));
            }
        };

        let has_path = rule.contains_key("path");
        let hostname = rule.get("hostname").and_then(Value::as_str);
        let is_canonical = !has_path
            && hostname.is_some_and(|hostname| hostname.eq_ignore_ascii_case(&canonical_hostname));

        if is_canonical {
            if canonical.is_none() {
                rule.insert("service", Value::String(canonical_service.clone()));
                canonical = Some(rule);
            }
            continue;
        }

        if !rule.contains_key("hostname") {
            let is_fallback = rule
                .get("service")
                .and_then(Value::as_str)
                .is_some_and(|service| service == "http_status:404");
            if !is_fallback {
                return Err(FwError::InvalidIngress(format!(
                    "ingress[{index}] is a hostless catch-all whose service is not `http_status:404`"
                )));
            }
            if fallback.is_none() {
                fallback = Some(rule);
            }
            continue;
        }

        specific.push(Value::Mapping(rule));
    }

    let canonical = canonical.unwrap_or_else(|| {
        mapping([
            ("hostname", Value::String(canonical_hostname)),
            ("service", Value::String(canonical_service)),
        ])
    });
    let fallback =
        fallback.unwrap_or_else(|| mapping([("service", Value::String("http_status:404".into()))]));

    specific.push(Value::Mapping(canonical));
    specific.push(Value::Mapping(fallback));
    if had_ingress {
        *root.get_mut("ingress").expect("existing ingress key") = Value::Sequence(specific);
    } else {
        root.insert("ingress", Value::Sequence(specific));
    }
    Ok(document)
}

fn require_regular_file(name: &'static str, path: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) | Err(_) => Err(FwError::MissingSibling {
            name,
            path: path.to_path_buf(),
        }),
    }
}

fn mapping<const N: usize>(entries: [(&str, Value); N]) -> Mapping {
    let mut mapping = Mapping::new();
    for (name, value) in entries {
        mapping.insert(name, value);
    }
    mapping
}

struct TempCandidate {
    path: PathBuf,
    armed: bool,
}

impl TempCandidate {
    fn create(destination: &Path, contents: &[u8]) -> Result<Self> {
        let parent = destination
            .parent()
            .ok_or(FwError::InvalidExecutableDirectory)?;
        let filename = destination
            .file_name()
            .ok_or(FwError::InvalidExecutableDirectory)?
            .to_string_lossy();

        for _ in 0..100 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".{filename}.{}.{}.tmp",
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(mut file) => {
                    if let Err(error) = write_and_flush(&mut file, contents) {
                        drop(file);
                        let _ = fs::remove_file(&candidate);
                        return Err(error);
                    }
                    return Ok(Self {
                        path: candidate,
                        armed: true,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(FwError::Other(
            "could not create a unique temporary Cloudflare config".into(),
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempCandidate {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn write_and_flush(file: &mut File, contents: &[u8]) -> Result<()> {
    file.write_all(contents)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    crate::platform::windows::atomic_replace(source, destination)
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str, port: u16) -> Value {
        serde_yml::from_str(&normalize_yaml(input, port).unwrap()).unwrap()
    }

    fn ingress(value: &Value) -> &[Value] {
        value
            .as_mapping()
            .unwrap()
            .get("ingress")
            .unwrap()
            .as_sequence()
            .unwrap()
    }

    fn field<'a>(rule: &'a Value, name: &str) -> Option<&'a Value> {
        rule.as_mapping().unwrap().get(name)
    }

    #[test]
    fn normalizes_order_and_preserves_unknown_values() {
        let value = parsed(
            r#"
tunnel: abc
originRequest:
  nested: [one, {two: 2}]
ingress:
  - hostname: "*.FW.RLZY.ME"
    service: http://127.0.0.1:3000
    originRequest:
      noTLSVerify: true
  - hostname: fuah.fw.rlzy.me
    service: http://127.0.0.1:6767
    unknown: [a, b]
  - service: http_status:404
    fallback-extra: keep
  - hostname: damn.fw.rlzy.me
    service: http://127.0.0.1:6769
"#,
            15432,
        );

        let root = value.as_mapping().unwrap();
        assert_eq!(root.get("tunnel").unwrap().as_str(), Some("abc"));
        assert!(root.get("originRequest").unwrap().is_mapping());

        let rules = ingress(&value);
        assert_eq!(rules.len(), 4);
        assert_eq!(
            field(&rules[0], "hostname").unwrap().as_str(),
            Some("fuah.fw.rlzy.me")
        );
        assert!(field(&rules[0], "unknown").unwrap().is_sequence());
        assert_eq!(
            field(&rules[1], "hostname").unwrap().as_str(),
            Some("damn.fw.rlzy.me")
        );
        assert_eq!(
            field(&rules[2], "service").unwrap().as_str(),
            Some("http://127.0.0.1:15432")
        );
        assert!(field(&rules[2], "originRequest").unwrap().is_mapping());
        assert_eq!(
            field(&rules[3], "service").unwrap().as_str(),
            Some("http_status:404")
        );
        assert_eq!(
            field(&rules[3], "fallback-extra").unwrap().as_str(),
            Some("keep")
        );
    }

    #[test]
    fn preserves_top_level_key_order() {
        let normalized = normalize_yaml(
            "first: 1\ningress:\n  - service: http_status:404\nlast: 2\n",
            10000,
        )
        .unwrap();
        let first = normalized.find("first:").unwrap();
        let ingress = normalized.find("ingress:").unwrap();
        let last = normalized.find("last:").unwrap();
        assert!(first < ingress && ingress < last);
    }

    #[test]
    fn creates_missing_ingress() {
        let value = parsed("tunnel: abc\ncredentials-file: credentials.json\n", 10000);
        let rules = ingress(&value);
        assert_eq!(rules.len(), 2);
        assert_eq!(
            field(&rules[0], "hostname").unwrap().as_str(),
            Some("*.fw.rlzy.me")
        );
        assert_eq!(
            field(&rules[0], "service").unwrap().as_str(),
            Some("http://127.0.0.1:10000")
        );
        assert_eq!(
            field(&rules[1], "service").unwrap().as_str(),
            Some("http_status:404")
        );
    }

    #[test]
    fn deduplicates_owned_wildcard_and_fallback() {
        let value = parsed(
            r#"
ingress:
  - hostname: '*.fw.rlzy.me'
    service: first
    keep: yes
  - service: http_status:404
    keep-fallback: yes
  - hostname: '*.fw.rlzy.me'
    service: second
    discard: yes
  - service: http_status:404
    discard-fallback: yes
"#,
            23456,
        );
        let rules = ingress(&value);
        assert_eq!(rules.len(), 2);
        assert_eq!(field(&rules[0], "keep").unwrap().as_str(), Some("yes"));
        assert!(field(&rules[0], "discard").is_none());
        assert_eq!(
            field(&rules[1], "keep-fallback").unwrap().as_str(),
            Some("yes")
        );
        assert!(field(&rules[1], "discard-fallback").is_none());
    }

    #[test]
    fn wildcard_with_path_remains_specific_and_unchanged() {
        let value = parsed(
            r#"
ingress:
  - hostname: '*.fw.rlzy.me'
    path: /api/*
    service: http://127.0.0.1:9000
"#,
            34567,
        );
        let rules = ingress(&value);
        assert_eq!(rules.len(), 3);
        assert_eq!(field(&rules[0], "path").unwrap().as_str(), Some("/api/*"));
        assert_eq!(
            field(&rules[0], "service").unwrap().as_str(),
            Some("http://127.0.0.1:9000")
        );
        assert_eq!(
            field(&rules[1], "service").unwrap().as_str(),
            Some("http://127.0.0.1:34567")
        );
    }

    #[test]
    fn rejects_non_mapping_document() {
        let error = normalize_yaml("- one\n- two\n", 10000).unwrap_err();
        assert!(matches!(error, FwError::InvalidIngress(_)));
    }

    #[test]
    fn rejects_non_sequence_ingress() {
        let error = normalize_yaml("ingress: {}\n", 10000).unwrap_err();
        assert!(matches!(error, FwError::InvalidIngress(_)));
    }

    #[test]
    fn rejects_non_mapping_rule() {
        let error = normalize_yaml("ingress:\n  - nope\n", 10000).unwrap_err();
        assert!(
            matches!(error, FwError::InvalidIngress(message) if message.contains("ingress[0]"))
        );
    }

    #[test]
    fn rejects_non_404_hostless_catch_all() {
        let error =
            normalize_yaml("ingress:\n  - service: http://127.0.0.1:8080\n", 10000).unwrap_err();
        assert!(matches!(error, FwError::InvalidIngress(message) if message.contains("hostless")));
    }
}
