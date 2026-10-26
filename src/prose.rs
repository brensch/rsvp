//! Guest-facing wording, kept out of the code in `copy.yaml`.
//!
//! `config/copy.yaml` (compiled in) has every key. A `copy.yaml` next to wedding.toml overrides
//! any of them; whatever it leaves out falls back to the defaults.

use std::path::Path;

use anyhow::{Context, Result};
use maud::{Markup, PreEscaped, html};
use serde_yaml::Value;

const DEFAULTS: &str = include_str!("../config/copy.yaml");

static GLOBAL: std::sync::OnceLock<Prose> = std::sync::OnceLock::new();

/// The wording in use, for the few places with no config at hand (error pages).
pub fn global() -> &'static Prose {
    GLOBAL.get_or_init(Prose::default)
}

#[derive(Debug, Clone)]
pub struct Prose {
    root: Value,
}

impl Default for Prose {
    fn default() -> Self {
        Self { root: serde_yaml::from_str(DEFAULTS).expect("config/copy.yaml parses") }
    }
}

impl Prose {
    /// The defaults with `path` laid over them (if it exists).
    pub fn load(path: &Path) -> Result<Self> {
        let mut c = Self::default();
        if path.exists() {
            let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            let over: Value = serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
            merge(&mut c.root, over);
        }
        let _ = GLOBAL.set(c.clone());
        Ok(c)
    }

    fn get(&self, key: &str) -> Option<&Value> {
        key.split('.').try_fold(&self.root, |v, k| v.get(k))
    }

    /// One piece of text, e.g. `t("website.reply.title")`. A missing key shows as `[key]`
    /// rather than failing, so a typo is visible on the page instead of breaking it.
    pub fn t(&self, key: &str) -> String {
        match self.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            _ => {
                tracing::warn!(key, "missing text in copy.yaml");
                format!("[{key}]")
            }
        }
    }

    /// Text with `{placeholders}` filled in.
    pub fn f(&self, key: &str, vars: &[(&str, &str)]) -> String {
        fill(&self.t(key), vars)
    }

    /// A list of paragraphs (a YAML list, or a single string).
    pub fn list(&self, key: &str, vars: &[(&str, &str)]) -> Vec<String> {
        match self.get(key) {
            Some(Value::Sequence(items)) => items.iter().filter_map(|v| v.as_str()).map(|s| fill(s, vars)).collect(),
            Some(Value::String(s)) if !s.trim().is_empty() => vec![fill(s, vars)],
            _ => vec![],
        }
    }

    /// Optional text: None when it's missing or empty.
    pub fn opt(&self, key: &str) -> Option<String> {
        self.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
    }

    /// Option labels in their written order, e.g. the accommodation choices.
    pub fn options(&self, key: &str) -> Vec<(String, String)> {
        match self.get(key) {
            Some(Value::Mapping(m)) => m
                .iter()
                .filter_map(|(k, v)| Some((k.as_str().map(String::from).unwrap_or_else(|| yaml_key(k)), v.as_str()?.to_string())))
                .collect(),
            _ => vec![],
        }
    }

    /// Everything, for the card templates.
    pub fn json(&self) -> serde_json::Value {
        serde_json::to_value(&self.root).unwrap_or(serde_json::Value::Null)
    }
}

fn yaml_key(k: &Value) -> String {
    match k {
        Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Mapping(b), Value::Mapping(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// The standard placeholders: {names} {a_first} {b_first} {date} {date_medium} {location}
/// {deadline} and, for a household, {greeting}.
pub fn vars(cfg: &crate::config::Config, greeting: &str) -> Vec<(&'static str, String)> {
    use crate::assets::words;
    let date = cfg.wedding_date().ok();
    vec![
        ("names", format!("{} & {}", cfg.couple.a_first, cfg.couple.b_first)),
        ("a_first", cfg.couple.a_first.clone()),
        ("b_first", cfg.couple.b_first.clone()),
        ("date", date.map(words::date_long).unwrap_or_default()),
        ("date_medium", date.map(words::date_medium).unwrap_or_default()),
        ("location", cfg.wedding.location.clone()),
        ("deadline", cfg.deadline().ok().map(words::date_medium).unwrap_or_default()),
        ("greeting", greeting.to_string()),
    ]
}

/// Borrow `vars()` in the shape `f` and `list` take.
pub fn borrow<'a>(owned: &'a [(&'static str, String)]) -> Vec<(&'a str, &'a str)> {
    owned.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

pub fn fill(s: &str, vars: &[(&str, &str)]) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Text for the website with `**bold**` made bold. Everything else is escaped.
pub fn rich(s: &str) -> Markup {
    let escaped = html! { (s) }.into_string();
    let mut out = String::new();
    for (i, part) in escaped.split("**").enumerate() {
        if i % 2 == 1 {
            out.push_str("<strong>");
            out.push_str(part);
            out.push_str("</strong>");
        } else {
            out.push_str(part);
        }
    }
    PreEscaped(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_overrides_and_placeholders() {
        let mut c = Prose::default();
        assert_eq!(c.t("website.reply.title"), "Your reply");
        merge(&mut c.root, serde_yaml::from_str("website:\n  reply:\n    title: Over to you\n").unwrap());
        assert_eq!(c.t("website.reply.title"), "Over to you");
        assert_eq!(c.t("website.reply.accept"), "Joyfully accepts", "untouched keys keep their default");
        assert_eq!(c.f("website.events.dress", &[("dress", "Formal")]), "Dress: Formal");
        assert_eq!(c.t("no.such.key"), "[no.such.key]");
        let opts = c.options("website.reply.transport_options");
        assert_eq!(opts[0], ("yes".to_string(), "Yes please".to_string()));
        assert_eq!(rich("by **1 April** <b>").into_string(), "by <strong>1 April</strong> &lt;b&gt;");
    }

    /// Every key the code asks for exists in config/copy.yaml.
    #[test]
    fn every_key_used_exists() {
        let c = Prose::default();
        let sources = [
            include_str!("web/guest.rs"),
            include_str!("web/site.rs"),
            include_str!("web/ui.rs"),
            include_str!("email.rs"),
            include_str!("rsvp.rs"),
            include_str!("address.rs"),
        ];
        let re = [".t(\"", ".f(\"", ".list(\"", ".options(\"", ".opt(\""];
        for src in sources {
            for pat in re {
                for (i, _) in src.match_indices(pat) {
                    let rest = &src[i + pat.len()..];
                    let key = &rest[..rest.find('"').unwrap()];
                    if key.contains('.') {
                        assert!(c.get(key).is_some(), "copy.yaml is missing `{key}`");
                    }
                }
            }
        }
    }
}
