//! The parts of Claude Code's `.claude.json` this tool reads.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::limits::{Reading, WindowKind};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Raw {
    oauth_account: Option<Value>,
    cached_usage_utilization: Option<Value>,
    additional_model_costs_cache: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Account {
    pub account_uuid: String,
    pub organization_uuid: Option<String>,
    pub organization_name: Option<String>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub plan: Option<String>,
    pub rate_limit_tier: Option<String>,
    pub billing_type: Option<String>,
    /// When Claude Code last fetched this profile; a login newer than this is not yet confirmed.
    pub profile_fetched_at: Option<DateTime<Utc>>,
}

impl Account {
    fn from_value(v: &Value) -> Option<Account> {
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
        };
        Some(Account {
            account_uuid: s("accountUuid")?,
            organization_uuid: s("organizationUuid"),
            organization_name: s("organizationName"),
            email: s("emailAddress"),
            display_name: s("displayName").or_else(|| s("fullName")),
            plan: s("planDisplayName").or_else(|| s("seatTier")),
            rate_limit_tier: s("userRateLimitTier").or_else(|| s("organizationRateLimitTier")),
            billing_type: s("billingType"),
            profile_fetched_at: v
                .get("profileFetchedAt")
                .and_then(Value::as_i64)
                .and_then(|ms| Utc.timestamp_millis_opt(ms).single()),
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Utilization {
    pub account_uuid: String,
    pub fetched_at: DateTime<Utc>,
    pub readings: Vec<Reading>,
    /// Extra (pay-as-you-go) usage, when enabled for the account.
    pub extra_usage_enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ClaudeJson {
    pub account: Option<Account>,
    /// The raw `oauthAccount` object, copied verbatim by the vault on a switch.
    pub oauth_account_raw: Option<Value>,
    pub utilization: Option<Utilization>,
    pub model_costs: Option<Value>,
}

pub fn read(path: &Path) -> anyhow::Result<ClaudeJson> {
    let text = std::fs::read_to_string(path)?;
    parse(&text)
}

pub fn parse(text: &str) -> anyhow::Result<ClaudeJson> {
    let raw: Raw = serde_json::from_str(text)?;
    Ok(ClaudeJson {
        account: raw.oauth_account.as_ref().and_then(Account::from_value),
        oauth_account_raw: raw.oauth_account,
        utilization: raw
            .cached_usage_utilization
            .as_ref()
            .and_then(parse_utilization),
        model_costs: raw.additional_model_costs_cache,
    })
}

fn parse_utilization(v: &Value) -> Option<Utilization> {
    let account_uuid = v.get("accountUuid")?.as_str()?.to_string();
    let fetched_at = Utc
        .timestamp_millis_opt(v.get("fetchedAtMs")?.as_i64()?)
        .single()?;
    let windows = v.get("utilization")?.as_object()?;
    let mut readings = Vec::new();
    for (k, w) in windows {
        let Some(kind) = WindowKind::from_key(k) else {
            continue;
        };
        let (Some(u), Some(r)) = (
            w.get("utilization").and_then(Value::as_f64),
            w.get("resets_at").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Ok(resets_at) = DateTime::parse_from_rfc3339(r) else {
            continue;
        };
        readings.push(Reading {
            kind,
            utilization: u,
            resets_at: resets_at.with_timezone(&Utc),
            fetched_at,
        });
    }
    readings.sort_by_key(|r| r.kind);
    let extra_usage_enabled = windows
        .get("extra_usage")
        .and_then(|e| e.get("is_enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(Utilization {
        account_uuid,
        fetched_at,
        readings,
        extra_usage_enabled,
    })
}

/// Writes `oauthAccount` into a `.claude.json`, keeping every other key and the key order.
pub fn with_oauth_account(text: &str, account: Option<&Value>) -> anyhow::Result<String> {
    let mut v: serde_json::Map<String, Value> = serde_json::from_str(text)?;
    match account {
        Some(a) => {
            v.insert("oauthAccount".into(), a.clone());
        }
        None => {
            v.remove("oauthAccount");
        }
    }
    Ok(serde_json::to_string_pretty(&v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "numStartups": 3,
      "oauthAccount": {"accountUuid": "acc-1", "emailAddress": "a@example.com", "organizationUuid": "org-1",
                       "displayName": "Ann", "billingType": "stripe_subscription", "userRateLimitTier": "default_claude_max_20x"},
      "cachedUsageUtilization": {"fetchedAtMs": 1795348787083, "accountUuid": "acc-1",
        "utilization": {"five_hour": null,
                        "seven_day": {"utilization": 65, "resets_at": "2026-11-25T14:59:59.983904+00:00"},
                        "tangelo": null, "extra_usage": {"is_enabled": false}}},
      "additionalModelCostsCache": {"claude-opus-5-5": {"inputTokens": 4, "outputTokens": 20}}
    }"#;

    #[test]
    fn reads_account_utilization_and_costs() {
        let c = parse(SAMPLE).unwrap();
        let a = c.account.unwrap();
        assert_eq!(a.account_uuid, "acc-1");
        assert_eq!(a.email.as_deref(), Some("a@example.com"));
        let u = c.utilization.unwrap();
        assert_eq!(
            u.readings.len(),
            1,
            "null windows and unknown keys are skipped"
        );
        assert_eq!(u.readings[0].kind, WindowKind::SevenDay);
        assert_eq!(u.readings[0].utilization, 65.0);
        assert_eq!(
            u.readings[0].resets_at.to_rfc3339(),
            "2026-11-25T14:59:59.983904+00:00"
        );
        // The window is named by its reset to the minute (sources differ by jitter).
        assert_eq!(
            u.readings[0].window().start.to_rfc3339(),
            "2026-11-18T15:00:00+00:00"
        );
        assert!(c.model_costs.is_some());
    }

    #[test]
    fn replaces_only_oauth_account() {
        let new = serde_json::json!({"accountUuid": "acc-2"});
        let out = with_oauth_account(SAMPLE, Some(&new)).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["oauthAccount"]["accountUuid"], "acc-2");
        assert_eq!(v["numStartups"], 3);
        assert_eq!(v["cachedUsageUtilization"]["accountUuid"], "acc-1");
        // Key order is kept (serde_json `preserve_order`).
        assert!(
            out.trim_start().starts_with("{\n  \"numStartups\""),
            "{out}"
        );
    }
}
