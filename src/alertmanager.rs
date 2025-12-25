use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct AlertManagerWebhook {
    pub version: String,
    #[serde(rename = "groupKey")]
    pub group_key: String,
    pub status: String,
    pub receiver: String,
    #[serde(rename = "groupLabels")]
    pub group_labels: HashMap<String, String>,
    #[serde(rename = "commonLabels")]
    pub common_labels: HashMap<String, String>,
    #[serde(rename = "commonAnnotations")]
    pub common_annotations: HashMap<String, String>,
    #[serde(rename = "externalURL")]
    pub external_url: String,
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Alert {
    pub status: String,
    pub labels: HashMap<String, String>,
    pub annotations: HashMap<String, String>,
    #[serde(rename = "startsAt")]
    pub starts_at: String,
    #[serde(rename = "endsAt")]
    pub ends_at: String,
    #[serde(rename = "generatorURL")]
    pub generator_url: String,
    pub fingerprint: String,
}

pub fn format_alert_message(webhook: &AlertManagerWebhook) -> String {
    // Format: "FIRING: AlertName (Severity) - Summary"
    // We'll take the first alert or summarize.
    // SMS length is limited (160 chars typically, but multipart is supported).
    // Let's try to be concise.

    let status = webhook.status.to_uppercase();
    let alert_name = webhook.common_labels.get("alertname").map(|s| s.as_str()).unwrap_or("Unknown Alert");
    let severity = webhook.common_labels.get("severity").map(|s| s.as_str()).unwrap_or("unknown");
    let summary = webhook.common_annotations.get("summary")
        .or_else(|| webhook.common_annotations.get("description"))
        .or_else(|| webhook.common_annotations.get("message"))
        .map(|s| s.as_str())
        .unwrap_or("No summary");

    format!("{}: {} ({}) - {}", status, alert_name, severity, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_alert() {
        let json = r#"{
  "version": "4",
  "groupKey": "{}:{alertname=\"TestAlert\"}",
  "truncatedAlerts": 0,
  "status": "firing",
  "receiver": "webhook",
  "groupLabels": {},
  "commonLabels": {
    "alertname": "TestAlert",
    "severity": "critical"
  },
  "commonAnnotations": {
    "summary": "Something is broken"
  },
  "externalURL": "http://localhost:9093",
  "alerts": []
}"#;
        let webhook: AlertManagerWebhook = serde_json::from_str(json).unwrap();
        let msg = format_alert_message(&webhook);
        assert_eq!(msg, "FIRING: TestAlert (critical) - Something is broken");
    }
}
