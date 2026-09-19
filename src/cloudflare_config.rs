use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_yml::{Mapping, Value};

use crate::cloudflared;
use crate::config::{
    CLOUDFLARE_CONFIG_FILENAME, CLOUDFLARE_DIRECTORY, CLOUDFLARED_FILENAME, LOOPBACK_HOST,
    MIN_PROXY_PORT,
};
use crate::error::{FwError, Result};
use crate::platform::{executable_directory_from, executable_name_from};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedConfig {
    pub yaml: String,
    pub base_domain: String,
}

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
        Self::from_executable(crate::platform::executable_path()?)
    }

    pub fn from_executable(fw_executable: PathBuf) -> Result<Self> {
        let executable_dir = executable_directory_from(&fw_executable)?;
        let cloudflare_dir =
            select_cloudflare_directory(&executable_dir, crate::platform::user_data_directory)?;
        let install_dir = cloudflare_dir
            .parent()
            .map(Path::to_path_buf)
            .ok_or(FwError::InvalidExecutableDirectory)?;
        let cloudflared = cloudflare_dir.join(CLOUDFLARED_FILENAME);
        let cloudflare_config = cloudflare_dir.join(CLOUDFLARE_CONFIG_FILENAME);

        require_cloudflare_directory(&cloudflare_dir, &fw_executable)?;
        require_regular_file(CLOUDFLARED_FILENAME, &cloudflared, &fw_executable)?;
        require_regular_file(
            CLOUDFLARE_CONFIG_FILENAME,
            &cloudflare_config,
            &fw_executable,
        )?;

        Ok(Self {
            fw_executable,
            install_dir,
            cloudflare_dir,
            cloudflared,
            cloudflare_config,
        })
    }
}

fn select_cloudflare_directory(
    install_dir: &Path,
    user_data_directory: impl FnOnce() -> Result<PathBuf>,
) -> Result<PathBuf> {
    let adjacent = install_dir.join(CLOUDFLARE_DIRECTORY);
    if adjacent.try_exists()? {
        Ok(adjacent)
    } else {
        Ok(user_data_directory()?.join(CLOUDFLARE_DIRECTORY))
    }
}

/// Checks the managed ingress contract without changing `config.yml`.
pub fn preflight_validate(paths: &InstallPaths) -> Result<()> {
    require_cloudflare_directory(&paths.cloudflare_dir, &paths.fw_executable)?;
    require_regular_file(
        CLOUDFLARED_FILENAME,
        &paths.cloudflared,
        &paths.fw_executable,
    )?;
    require_regular_file(
        CLOUDFLARE_CONFIG_FILENAME,
        &paths.cloudflare_config,
        &paths.fw_executable,
    )?;
    let input = fs::read_to_string(&paths.cloudflare_config)?;
    normalize_yaml(&input, MIN_PROXY_PORT)?;
    Ok(())
}

/// Normalizes, validates, and atomically installs the managed Cloudflare config.
///
/// Both files in the selected `cf` directory must already have been validated
/// through [`InstallPaths`].
/// The original config remains untouched unless the official validator accepts
/// the complete temporary candidate.
pub async fn normalize_validate_and_replace(
    paths: &InstallPaths,
    proxy_port: u16,
) -> Result<String> {
    // Recheck immediately before mutation in case installation files changed
    // after startup path resolution.
    require_cloudflare_directory(&paths.cloudflare_dir, &paths.fw_executable)?;
    require_regular_file(
        CLOUDFLARED_FILENAME,
        &paths.cloudflared,
        &paths.fw_executable,
    )?;
    require_regular_file(
        CLOUDFLARE_CONFIG_FILENAME,
        &paths.cloudflare_config,
        &paths.fw_executable,
    )?;

    let input = fs::read_to_string(&paths.cloudflare_config)?;
    let normalized = normalize_yaml(&input, proxy_port)?;
    let mut candidate =
        TempCandidate::create(&paths.cloudflare_config, normalized.yaml.as_bytes())?;

    cloudflared::validate_config(&paths.cloudflared, candidate.path(), &paths.cloudflare_dir)
        .await?;
    atomic_replace(candidate.path(), &paths.cloudflare_config)?;
    candidate.disarm();
    Ok(normalized.base_domain)
}

pub fn normalize_yaml(input: &str, proxy_port: u16) -> Result<NormalizedConfig> {
    let document: Value = serde_yml::from_str(input)
        .map_err(|error| FwError::InvalidIngress(format!("Invalid YAML: {error}")))?;
    let (normalized, base_domain) = normalize_value(document, proxy_port)?;
    Ok(NormalizedConfig {
        yaml: serde_yml::to_string(&normalized)?,
        base_domain,
    })
}

pub fn normalize_value(mut document: Value, proxy_port: u16) -> Result<(Value, String)> {
    let root = document.as_mapping_mut().ok_or_else(|| {
        FwError::InvalidIngress("the YAML document must be a top-level mapping".into())
    })?;

    let entries = match root.get_mut("ingress") {
        Some(Value::Sequence(entries)) => std::mem::take(entries),
        Some(_) => {
            return Err(FwError::InvalidIngress(
                "`ingress` must be a sequence".into(),
            ));
        }
        None => Vec::new(),
    };

    let canonical_service = format!("http://{LOOPBACK_HOST}:{proxy_port}");

    let mut specific = Vec::with_capacity(entries.len());
    let mut managed = None;
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

        let hostname = rule
            .get("hostname")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if !rule.contains_key("path") {
            if let Some(hostname) = hostname.as_deref() {
                if let Some(base_domain) = wildcard_base_domain(hostname, index)? {
                    if let Some((first_index, first_hostname, _)) = &managed {
                        return Err(FwError::InvalidIngress(format!(
                            "Ingress rules contain ambiguous multiple wildcard hostnames. Expected exactly one.\n\n\
                             Detected ingress[{first_index}]: {first_hostname}\n\
                             Detected ingress[{index}]: {hostname}\n\n\
                             Please choose one as the intended FW wildcard hostname and remove the others."
                        )));
                    }
                    rule.insert("service", Value::String(canonical_service.clone()));
                    managed = Some((index, hostname.to_owned(), (rule, base_domain)));
                    continue;
                }
            }
        }

        specific.push(Value::Mapping(rule));
    }

    let (_, _, (managed, base_domain)) = managed.ok_or_else(|| {
        FwError::InvalidIngress(
            "No pathless wildcard hostname was found. Expected exactly one rule such as \
             `hostname: \"*.example.com\"`."
                .into(),
        )
    })?;
    let fallback =
        fallback.unwrap_or_else(|| mapping([("service", Value::String("http_status:404".into()))]));

    specific.push(Value::Mapping(managed));
    specific.push(Value::Mapping(fallback));
    *root.get_mut("ingress").expect("wildcard requires ingress") = Value::Sequence(specific);
    Ok((document, base_domain))
}

fn wildcard_base_domain(hostname: &str, index: usize) -> Result<Option<String>> {
    let Some(domain) = hostname.strip_prefix("*.") else {
        return Ok(None);
    };
    let domain = domain.to_ascii_lowercase();
    let valid = !domain.is_empty()
        && domain.len() <= 253
        && domain.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if !valid {
        return Err(FwError::InvalidIngress(format!(
            "Ingress[{index}] contains an invalid wildcard hostname `{hostname}`."
        )));
    }
    Ok(Some(domain))
}

fn require_cloudflare_directory(path: &Path, fw_executable: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) | Err(_) => Err(FwError::MissingCloudflareDirectory {
            executable_name: executable_name_from(fw_executable)?,
            path: path.to_path_buf(),
            setup_eligible: crate::setup::is_eligible(),
        }),
    }
}

fn require_regular_file(name: &'static str, path: &Path, fw_executable: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) | Err(_) => Err(FwError::MissingSibling {
            name,
            executable_name: executable_name_from(fw_executable)?,
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

fn atomic_replace(source: &Path, destination: &Path) -> Result<()> {
    crate::platform::atomic_replace(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_directory_prefers_an_existing_adjacent_directory() {
        let install_dir = std::env::temp_dir().join(format!(
            "fw-config-path-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let adjacent = install_dir.join(CLOUDFLARE_DIRECTORY);
        fs::create_dir_all(&adjacent).unwrap();

        assert_eq!(
            select_cloudflare_directory(&install_dir, || panic!("user data must stay lazy"))
                .unwrap(),
            adjacent
        );
        fs::remove_dir_all(install_dir).unwrap();
    }

    #[test]
    fn configuration_directory_uses_user_data_when_adjacent_is_absent() {
        let install_dir = std::env::temp_dir().join(format!(
            "fw-config-path-missing-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let user_data = PathBuf::from("user-data");

        assert_eq!(
            select_cloudflare_directory(&install_dir, || Ok(user_data.clone())).unwrap(),
            user_data.join(CLOUDFLARE_DIRECTORY)
        );
    }

    fn parsed(input: &str, port: u16) -> Value {
        serde_yml::from_str(&normalize_yaml(input, port).unwrap().yaml).unwrap()
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
            "first: 1\ningress:\n  - hostname: '*.example.com'\n    service: old\n  - service: http_status:404\nlast: 2\n",
            10000,
        )
        .unwrap();
        let first = normalized.yaml.find("first:").unwrap();
        let ingress = normalized.yaml.find("ingress:").unwrap();
        let last = normalized.yaml.find("last:").unwrap();
        assert!(first < ingress && ingress < last);
    }

    #[test]
    fn detects_domain_and_creates_missing_fallback() {
        let normalized = normalize_yaml(
            "ingress:\n  - hostname: '*.MYTUNNEL.ME'\n    service: old\n",
            10000,
        )
        .unwrap();
        assert_eq!(normalized.base_domain, "mytunnel.me");

        let value: Value = serde_yml::from_str(&normalized.yaml).unwrap();
        let rules = ingress(&value);
        assert_eq!(rules.len(), 2);
        assert_eq!(
            field(&rules[0], "hostname").unwrap().as_str(),
            Some("*.MYTUNNEL.ME")
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
    fn rejects_multiple_wildcard_hostnames() {
        let error = normalize_yaml(
            r#"
ingress:
  - hostname: '*.fw.rlzy.me'
    service: first
  - hostname: '*.mytunnel.me'
    service: second
"#,
            23456,
        )
        .unwrap_err();
        assert!(matches!(error, FwError::InvalidIngress(message)
            if message.contains("ambiguous multiple wildcard hostnames")
                && message.contains("Detected ingress[0]: *.fw.rlzy.me")
                && message.contains("Detected ingress[1]: *.mytunnel.me")));
    }

    #[test]
    fn wildcard_with_path_remains_specific_and_unchanged() {
        let value = parsed(
            r#"
ingress:
  - hostname: '*.fw.rlzy.me'
    path: /api/*
    service: http://127.0.0.1:9000
  - hostname: '*.mytunnel.me'
    service: old
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
    fn rejects_missing_wildcard_hostname() {
        let error = normalize_yaml(
            "ingress:\n  - hostname: apple-cat.example.com\n    service: local\n  - service: http_status:404\n",
            10000,
        )
        .unwrap_err();
        assert!(matches!(error, FwError::InvalidIngress(message)
            if message.contains("No pathless wildcard hostname")));
    }

    #[test]
    fn rejects_invalid_wildcard_hostname() {
        let error = normalize_yaml(
            "ingress:\n  - hostname: '*.*.example.com'\n    service: local\n",
            10000,
        )
        .unwrap_err();
        assert!(matches!(error, FwError::InvalidIngress(message)
            if message.contains("invalid wildcard hostname")));
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
