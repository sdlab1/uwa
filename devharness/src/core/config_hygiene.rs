//! Гигиена конфига: секреты не попадают в .harness/config и в логи.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    pub key: String,
    pub line: usize,
}

pub const SECRET_KEYS: &[&str] = &[
    "DEVH_LEAD_PASSWORD",
    "DEVH_AGENT_API_KEY",
    "DEVH_REMOTE_TOKEN",
];

pub fn mask_value(value: &str) -> String {
    if value.len() <= 2 {
        "*".repeat(value.len())
    } else {
        format!("*{}", &value[value.len() - 1..])
    }
    .replace(|c: char| c.is_ascii_alphanumeric(), "*")
    .chars()
    .map(|c| if c == '*' { '*' } else { c })
    .collect()
}

pub fn mask_line(line: &str) -> String {
    let parts: Vec<&str> = line.splitn(2, '=').collect();
    if parts.len() < 2 {
        return line.to_string();
    }
    let key = parts[0].trim();
    let val = parts[1].trim();
    if SECRET_KEYS.iter().any(|s| key.eq_ignore_ascii_case(s)) {
        format!("{}=***MASKED***", key)
    } else {
        format!("{key}={val}")
    }
}

pub fn detect_in_config(text: &str) -> Vec<SecretRef> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            let key = key.trim();
            if SECRET_KEYS.iter().any(|s| key.eq_ignore_ascii_case(s)) {
                out.push(SecretRef {
                    key: key.to_string(),
                    line: i + 1,
                });
            }
        }
    }
    out
}

pub fn sanitize_config(text: &str) -> String {
    let mut out = vec![];
    for line in text.lines() {
        out.push(mask_line(line));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_value_hides_secret() {
        let m = mask_value("supersecret123");
        assert!(
            !m.contains("supersecret123"),
            "секрет не должен остаться в маске: {m}"
        );
    }

    #[test]
    fn mask_line_redacts_lead_password() {
        let masked = mask_line("DEVH_LEAD_PASSWORD=abc123secret");
        assert_eq!(masked, "DEVH_LEAD_PASSWORD=***MASKED***");
    }

    #[test]
    fn mask_line_preserves_non_secret() {
        let masked = mask_line("VERIFY=true");
        assert_eq!(masked, "VERIFY=true");
    }

    #[test]
    fn detect_finds_secrets_in_config() {
        let config = "\
# комментарий
VERIFY=true
DEVH_LEAD_PASSWORD=hunter2
AGENT=goose run
DEVH_AGENT_API_KEY=sk-12345";
        let found = detect_in_config(config);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|r| r.key == "DEVH_LEAD_PASSWORD"));
        assert!(found.iter().any(|r| r.key == "DEVH_AGENT_API_KEY"));
    }

    #[test]
    fn sanitize_config_redacts_all_secrets() {
        let config = "\
DEVH_LEAD_PASSWORD=secret123
VERIFY=true
DEVH_REMOTE_TOKEN=token_value";
        let sanitized = sanitize_config(config);
        assert!(!sanitized.contains("secret123"));
        assert!(!sanitized.contains("token_value"));
        assert!(sanitized.contains("VERIFY=true"));
    }

    #[test]
    fn detect_empty_config() {
        let found = detect_in_config("");
        assert!(found.is_empty());
    }
}
