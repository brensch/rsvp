//! Guest-supplied mailing addresses, checked server-side with Google's Address
//! Validation API (no third-party JavaScript on the page). Saved through the same
//! journal → database path as RSVPs.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::journal::{Entry, Journal};
use crate::store::{self, Household};

/// Countries offered in the form: (display name, CLDR region code).
pub const COUNTRIES: &[(&str, &str)] = &[
    ("United States", "US"),
    ("Australia", "AU"),
    ("New Zealand", "NZ"),
    ("United Kingdom", "GB"),
    ("Canada", "CA"),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Address {
    pub addr1: String,
    pub addr2: String,
    pub city: String,
    pub region: String,
    pub postcode: String,
    pub country: String,
}

impl Address {
    pub fn from_household(h: &Household) -> Self {
        Self {
            addr1: h.addr1.clone(),
            addr2: h.addr2.clone(),
            city: h.city.clone(),
            region: h.region.clone(),
            postcode: h.postcode.clone(),
            country: h.country.clone(),
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let locality = [self.city.trim(), self.region.trim(), self.postcode.trim()]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        [self.addr1.trim(), self.addr2.trim(), &locality, self.country.trim()]
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    pub fn region_code(&self) -> Option<&'static str> {
        let c = self.country.trim().to_lowercase();
        COUNTRIES.iter().find(|(name, code)| c == name.to_lowercase() || c == code.to_lowercase()).map(|(_, code)| *code)
            .or(match c.as_str() {
                "usa" | "united states of america" => Some("US"),
                "uk" | "england" | "scotland" | "wales" => Some("GB"),
                _ => None,
            })
    }

    pub fn validate_fields(&self) -> Result<(), String> {
        if self.addr1.trim().is_empty() {
            return Err(crate::prose::global().t("website.address.need_street"));
        }
        if self.city.trim().is_empty() {
            return Err(crate::prose::global().t("website.address.need_city"));
        }
        if self.country.trim().is_empty() {
            return Err(crate::prose::global().t("website.address.need_country"));
        }
        for v in [&self.addr1, &self.addr2, &self.city, &self.region, &self.postcode, &self.country] {
            if v.len() > 200 {
                return Err(crate::prose::global().t("website.address.too_long"));
            }
        }
        Ok(())
    }

    /// Loose comparison: case, punctuation, whitespace, ZIP+4 and common abbreviations don't count.
    pub fn same_as(&self, other: &Address) -> bool {
        fn norm(s: &str) -> String {
            let s: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
            s.split_whitespace()
                .map(|w| match w {
                    "street" => "st",
                    "road" => "rd",
                    "avenue" => "ave",
                    "drive" => "dr",
                    "parkway" => "pkwy",
                    "boulevard" => "blvd",
                    "court" => "ct",
                    "place" => "pl",
                    "lane" => "ln",
                    "apartment" | "unit" | "suite" | "apt" => "#",
                    other => other,
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
        let zip = |s: &str| norm(s).split(' ').next().unwrap_or("").to_string();
        norm(&self.addr1) == norm(&other.addr1)
            && norm(&self.addr2) == norm(&other.addr2)
            && norm(&self.city) == norm(&other.city)
            && norm(&self.region) == norm(&other.region)
            && zip(&self.postcode) == zip(&other.postcode)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Google is confident; use its version.
    Accept,
    /// Plausible but Google changed or inferred something worth showing.
    Confirm,
    /// Building has units and none was given.
    AddUnit,
    /// Google couldn't confirm it.
    Fix,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub action: Action,
    pub suggestion: Address,
    pub unconfirmed: Vec<String>,
    pub missing: Vec<String>,
}

pub async fn check(http: &reqwest::Client, key: &str, input: &Address) -> Result<Option<Check>> {
    let Some(region) = input.region_code() else { return Ok(None) };
    let mut lines = vec![input.addr1.trim().to_string()];
    if !input.addr2.trim().is_empty() {
        lines.push(input.addr2.trim().to_string());
    }
    let mut body = json!({"address": {
        "regionCode": region,
        "addressLines": lines,
        "locality": input.city.trim(),
        "administrativeArea": input.region.trim(),
        "postalCode": input.postcode.trim(),
    }});
    if region == "US" {
        body["enableUspsCass"] = json!(true);
    }
    let resp = http
        .post("https://addressvalidation.googleapis.com/v1:validateAddress")
        .query(&[("key", key)])
        .timeout(Duration::from_secs(8))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        bail!("address validation {status}: {}", v["error"]["message"].as_str().unwrap_or(""));
    }
    let r = &v["result"];
    let pa = &r["address"]["postalAddress"];
    let lines: Vec<String> = pa["addressLines"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let country = COUNTRIES.iter().find(|(_, c)| Some(*c) == pa["regionCode"].as_str()).map(|(n, _)| n.to_string()).unwrap_or_else(|| input.country.clone());
    let suggestion = Address {
        addr1: lines.first().cloned().unwrap_or_default(),
        addr2: lines.get(1..).map(|l| l.join(", ")).unwrap_or_default(),
        city: pa["locality"].as_str().unwrap_or("").to_string(),
        region: pa["administrativeArea"].as_str().unwrap_or("").to_string(),
        postcode: pa["postalCode"].as_str().unwrap_or("").to_string(),
        country,
    };
    let list = |k: &str| r["address"][k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.replace('_', " "))).collect()).unwrap_or_default();
    let action = match r["verdict"]["possibleNextAction"].as_str() {
        Some("ACCEPT") => Action::Accept,
        Some("CONFIRM_ADD_SUBPREMISES") => Action::AddUnit,
        Some("CONFIRM") => Action::Confirm,
        Some("FIX") => Action::Fix,
        // Older responses without possibleNextAction: fall back to the verdict flags.
        _ if r["verdict"]["addressComplete"].as_bool() == Some(true) && r["verdict"]["hasUnconfirmedComponents"].as_bool() != Some(true) => Action::Accept,
        _ => Action::Fix,
    };
    Ok(Some(Check { action, suggestion, unconfirmed: list("unconfirmedComponentTypes"), missing: list("missingComponentTypes") }))
}

/// What a guest submitted. Status: "verified" (Google's version), "guest-confirmed"
/// (guest kept their own after seeing a suggestion/warning), "unverified" (not checked).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub address: Address,
    pub status: String,
}

pub fn submit(conn: &Connection, journal: &Journal, h: &Household, sub: &Submission, ip_hash: &str, ua: &str) -> Result<()> {
    let entry = Entry {
        id: uuid::Uuid::now_v7().to_string(),
        kind: "address".into(),
        household_id: h.id,
        received_at: crate::db::now(),
        ip_hash: ip_hash.to_string(),
        user_agent: ua.chars().take(300).collect(),
        payload: serde_json::to_value(sub)?,
    };
    journal.append(&entry).context("writing address journal")?;
    crate::rsvp::apply_entry(conn, None, &entry, true)?;
    Ok(())
}

/// Called from `rsvp::apply_entry` inside its transaction.
pub fn apply(tx: &Connection, h: &Household, entry: &Entry, notify: bool) -> Result<()> {
    let sub: Submission = serde_json::from_value(entry.payload.clone()).context("address payload")?;
    let a = &sub.address;
    tx.execute(
        "UPDATE households SET addr1 = ?1, addr2 = ?2, city = ?3, region = ?4, postcode = ?5, country = ?6,
         address_status = ?7, address_at = ?8, updated_at = ?8 WHERE id = ?9",
        params![a.addr1.trim(), a.addr2.trim(), a.city.trim(), a.region.trim(), a.postcode.trim(), a.country.trim(), sub.status, entry.received_at, h.id],
    )?;
    store::audit(tx, &format!("guest:{}", h.id), "address.submit", "household", &h.id.to_string(), &json!({"submission": entry.id, "status": sub.status}))?;
    if notify {
        let first = h.address_lines().is_empty();
        let status = match sub.status.as_str() {
            "verified" => "✓ verified",
            "guest-confirmed" => "⚠ kept their own version",
            _ => "not checked",
        };
        store::enqueue(
            tx,
            "discord",
            &json!({"type": "alert", "text": format!("📮 **{}** {} their address ({status})\n{}", h.greeting, if first { "added" } else { "updated" }, a.lines().join(", "))}),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Search-as-you-type, proxied through the server (Places API, New).
// Session tokens group a guest's keystrokes + the final lookup for billing.

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub id: String,
    pub main: String,
    pub secondary: String,
}

pub fn valid_session(s: &str) -> bool {
    (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn valid_place_id(s: &str) -> bool {
    (10..=300).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub async fn autocomplete(http: &reqwest::Client, key: &str, input: &str, session: &str, prefer: &str) -> Result<Vec<Suggestion>> {
    let mut regions: Vec<String> = COUNTRIES.iter().map(|(_, c)| c.to_lowercase()).collect();
    // Put the household's likely country first; Google treats the list as a filter, not an order,
    // so we also bias with regionCode.
    regions.retain(|r| r != prefer);
    regions.insert(0, prefer.to_string());
    let resp = http
        .post("https://places.googleapis.com/v1/places:autocomplete")
        .header("X-Goog-Api-Key", key)
        .timeout(Duration::from_secs(5))
        .json(&json!({
            "input": input,
            "sessionToken": session,
            "includedRegionCodes": regions,
            "regionCode": prefer,
            "includedPrimaryTypes": ["street_address", "premise", "subpremise"],
        }))
        .send()
        .await?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        bail!("places autocomplete {status}: {}", v["error"]["message"].as_str().unwrap_or(""));
    }
    Ok(v["suggestions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let p = &s["placePrediction"];
                    Some(Suggestion {
                        id: p["placeId"].as_str()?.to_string(),
                        main: p["structuredFormat"]["mainText"]["text"].as_str().or(p["text"]["text"].as_str())?.to_string(),
                        secondary: p["structuredFormat"]["secondaryText"]["text"].as_str().unwrap_or("").to_string(),
                    })
                })
                .take(6)
                .collect()
        })
        .unwrap_or_default())
}

pub async fn place(http: &reqwest::Client, key: &str, id: &str, session: &str) -> Result<Address> {
    let resp = http
        .get(format!("https://places.googleapis.com/v1/places/{id}"))
        .query(&[("sessionToken", session)])
        .header("X-Goog-Api-Key", key)
        .header("X-Goog-FieldMask", "postalAddress")
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        bail!("place details {status}: {}", v["error"]["message"].as_str().unwrap_or(""));
    }
    let pa = &v["postalAddress"];
    let lines: Vec<String> = pa["addressLines"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let code = pa["regionCode"].as_str().unwrap_or("");
    Ok(Address {
        addr1: lines.first().cloned().unwrap_or_default(),
        addr2: lines.get(1..).map(|l| l.join(", ")).unwrap_or_default(),
        city: pa["locality"].as_str().unwrap_or("").to_string(),
        region: pa["administrativeArea"].as_str().unwrap_or("").to_string(),
        postcode: pa["postalCode"].as_str().unwrap_or("").to_string(),
        country: COUNTRIES.iter().find(|(_, c)| *c == code).map(|(n, _)| n.to_string()).unwrap_or_else(|| code.to_string()),
    })
}
