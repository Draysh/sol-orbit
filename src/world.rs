//! A world's manifest, `sol-world.json`: who the world is, what it stores,
//! what it tells other worlds, what it can do for them, and its settings.
//!
//! Each world publishes one in its repository; Sol reads it when the world is
//! installed and uses it to draw the world's settings form and the choices in
//! the connection builder.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WorldManifest {
    /// Lowercase letters, digits and dashes; also the world's namespace.
    pub id: String,
    pub name: String,
    /// One line about what it is for.
    pub tagline: String,
    /// `owner/name` on GitHub, where releases and the app downloads live.
    pub repo: String,
    /// The planet a moon belongs to, e.g. `neptune` for Triton.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// What it keeps in Sol, for the data overview.
    #[serde(default)]
    pub collections: Vec<Collection>,
    /// Events it posts, which connections can listen for.
    #[serde(default)]
    pub emits: Vec<EventKind>,
    /// Things it can do when another world's event arrives.
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Settings people set once in Sol; every paired device reads them.
    #[serde(default)]
    pub settings: Vec<Setting>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Collection {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventKind {
    #[serde(rename = "type")]
    pub kind: String,
    /// How the connection builder says it: `A habit is ticked`.
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Action {
    pub id: String,
    /// How the connection builder says it: `Tick a habit`.
    pub label: String,
    #[serde(default)]
    pub params: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Setting {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

/// A parameter of an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    /// An `http://` or `https://` address.
    Url,
    /// Kept in Sol and handed only to the world's own devices; never shown again.
    Secret,
    Number,
    Toggle,
    /// `HH:MM`, in the person's time zone.
    Time,
}

impl FieldKind {
    /// Checks a value against the kind; `Ok` carries the value to store.
    pub fn check(self, value: &serde_json::Value) -> Result<serde_json::Value, String> {
        use serde_json::Value;
        match (self, value) {
            (Self::Text | Self::Secret, Value::String(s)) if s.len() <= 4000 => Ok(value.clone()),
            (Self::Url, Value::String(s))
                if (s.starts_with("http://") || s.starts_with("https://")) && s.len() <= 2000 =>
            {
                Ok(Value::String(s.trim_end_matches('/').to_owned()))
            }
            (Self::Number, Value::Number(_)) | (Self::Toggle, Value::Bool(_)) => Ok(value.clone()),
            (Self::Time, Value::String(s)) if valid_time(s) => Ok(value.clone()),
            (Self::Url, _) => Err("use an address that starts with http:// or https://".into()),
            (Self::Time, _) => Err("use a time like 07:30".into()),
            (Self::Number, _) => Err("use a number".into()),
            (Self::Toggle, _) => Err("use on or off".into()),
            _ => Err("use some text (at most 4000 characters)".into()),
        }
    }
}

fn valid_time(s: &str) -> bool {
    let Some((h, m)) = s.split_once(':') else {
        return false;
    };
    h.len() == 2
        && m.len() == 2
        && h.parse::<u8>().is_ok_and(|h| h < 24)
        && m.parse::<u8>().is_ok_and(|m| m < 60)
}

/// Lowercase letters, digits and dashes, starting with a letter, at most 32.
pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl WorldManifest {
    /// Every problem with the manifest, so a broken one is explained at once.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !valid_id(&self.id) || self.id == "sol" {
            out.push(format!(
                "id {:?} must be lowercase letters, digits or dashes, and not \"sol\"",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            out.push("name is empty".into());
        }
        let repo_ok = self.repo.split_once('/').is_some_and(|(owner, name)| {
            !owner.is_empty()
                && !name.is_empty()
                && !name.contains('/')
                && self
                    .repo
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "/-_.".contains(c))
        });
        if !repo_ok {
            out.push(format!("repo {:?} should be owner/name", self.repo));
        }
        for e in &self.emits {
            if !crate::event::valid_type(&self.id, &e.kind) {
                out.push(format!(
                    "event type {:?} should be {}.<thing>.<verb>",
                    e.kind, self.id
                ));
            }
        }
        let mut keys = std::collections::HashSet::new();
        for s in &self.settings {
            if !keys.insert(&s.key) {
                out.push(format!("setting {:?} appears twice", s.key));
            }
            if let Some(default) = &s.default
                && let Err(why) = s.kind.check(default)
            {
                out.push(format!("default of setting {:?}: {why}", s.key));
            }
        }
        let mut actions = std::collections::HashSet::new();
        for a in &self.actions {
            if !actions.insert(&a.id) {
                out.push(format!("action {:?} appears twice", a.id));
            }
        }
        out
    }

    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.id == id)
    }

    pub fn emitted(&self, kind: &str) -> Option<&EventKind> {
        self.emits.iter().find(|e| e.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn manifest() -> WorldManifest {
        serde_json::from_value(json!({
            "id": "terra",
            "name": "Terra",
            "tagline": "Habits, diary and wellbeing",
            "repo": "Draysh/sol-terra",
            "emits": [{ "type": "terra.habit.checked", "label": "A habit is ticked" }],
            "actions": [{ "id": "tick-habit", "label": "Tick a habit",
                          "params": [{ "key": "habit", "label": "Habit", "kind": "text", "required": true }] }],
            "settings": [{ "key": "day_starts", "label": "Day starts at", "kind": "time", "default": "04:00" }]
        }))
        .unwrap()
    }

    #[test]
    fn a_good_manifest_has_no_problems() {
        assert!(
            manifest().problems().is_empty(),
            "{:?}",
            manifest().problems()
        );
        assert!(manifest().action("tick-habit").is_some());
    }

    #[test]
    fn problems_are_spelled_out() {
        let mut m = manifest();
        m.id = "Sol!".into();
        m.repo = "nope".into();
        m.emits[0].kind = "neptune.track.played".into();
        m.settings[0].default = Some(json!("25:00"));
        assert_eq!(m.problems().len(), 4, "{:?}", m.problems());
    }

    #[test]
    fn settings_are_checked_by_kind() {
        assert_eq!(
            FieldKind::Url.check(&json!("https://music.local/")),
            Ok(json!("https://music.local"))
        );
        assert!(FieldKind::Url.check(&json!("music.local")).is_err());
        assert!(FieldKind::Time.check(&json!("07:30")).is_ok());
        assert!(FieldKind::Number.check(&json!("7")).is_err());
        assert!(FieldKind::Toggle.check(&json!(true)).is_ok());
    }
}
