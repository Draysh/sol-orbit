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
    /// What it is for, in a word or three (`Habits and diary`): shown under
    /// its name in the app menu and the tray.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `owner/name` on GitHub, where releases and the app downloads live.
    pub repo: String,
    /// The planet a moon belongs to, e.g. `neptune` for Triton.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The world whose app carries this one, for a moon that has no app of
    /// its own (its pages are in its planet's): its `parent`. Sol offers no
    /// download for it, and lets the planet's paired device pair it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
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
    /// Connections this world works best with, offered in Sol once both
    /// worlds are installed: a moon lists the ones to and from its planet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<Suggested>,
    /// The oldest Sol the world works with, e.g. `1.2.0`; Sol won't install
    /// it when it is older.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_sol: Option<String>,
    /// The protocol its app speaks ([`crate::PROTOCOL`] when it was built);
    /// Sol won't install a world whose app it can't talk to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
}

/// A connection a world suggests: "when this happens there, do that here"
/// (or the other way round). Sol offers it, with `why`, wherever the person
/// can make it in one click; nothing is connected without them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Suggested {
    /// The event, e.g. `saturn.title.rated`; its world is the part before the first dot.
    pub when: String,
    /// The world whose action runs.
    pub then_world: String,
    /// Its action's id.
    pub then_action: String,
    /// The action's parameters, filled in.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
    /// One sentence on what it does for the person: `Titan learns what you rate.`
    pub why: String,
}

impl Suggested {
    /// The world the event comes from.
    pub fn when_world(&self) -> &str {
        self.when.split('.').next().unwrap_or_default()
    }
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
    /// One value for every world that names the same thing (`navidrome-url`):
    /// set in any of them, Sol keeps it once and hands it to all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<String>,
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

/// What a setting or a parameter holds. A kind this build doesn't know
/// (from a newer world) reads as [`FieldKind::Unknown`]: Sol shows such a
/// setting with a note and doesn't take values for it.
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
    /// A kind from a newer protocol.
    #[serde(other)]
    Unknown,
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
            (Self::Unknown, _) => Err(
                "this Sol doesn't know this kind of setting yet; update Sol to change it".into(),
            ),
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
        if self.app.is_some() && self.app != self.parent {
            out.push("app, when set, is the planet the moon belongs to (its parent)".into());
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
                && s.kind != FieldKind::Unknown
                && let Err(why) = s.kind.check(default)
            {
                out.push(format!("default of setting {:?}: {why}", s.key));
            }
            if let Some(shared) = &s.shared
                && !valid_id(shared)
            {
                out.push(format!(
                    "shared name {shared:?} of setting {:?} must be lowercase letters, digits or dashes",
                    s.key
                ));
            }
        }
        if let Some(min) = &self.min_sol
            && semver::Version::parse(min.trim_start_matches('v')).is_err()
        {
            out.push(format!("min_sol {min:?} should be a version like 1.0.0"));
        }
        let mut actions = std::collections::HashSet::new();
        for a in &self.actions {
            if !actions.insert(&a.id) {
                out.push(format!("action {:?} appears twice", a.id));
            }
        }
        out
    }

    /// Why a Sol of `version`, speaking protocols `min_protocol` to
    /// `protocol`, can't take this world, in a sentence for people; `None`
    /// when it can.
    pub fn unfit_for(&self, version: &str, min_protocol: u32, protocol: u32) -> Option<String> {
        if let Some(min) = &self.min_sol
            && crate::update::newer(min, version) == Some(true)
        {
            return Some(format!(
                "{} needs Sol {} or newer, and this Sol is {version}; update Sol first",
                self.name,
                min.trim_start_matches('v')
            ));
        }
        match self.protocol {
            Some(p) if p > protocol => Some(format!(
                "{}'s app speaks protocol {p}, newer than this Sol's ({protocol}); update Sol first",
                self.name
            )),
            Some(p) if p < min_protocol => Some(format!(
                "{}'s app speaks protocol {p}, which this Sol no longer speaks (it needs {min_protocol} or newer); the world needs a newer release",
                self.name
            )),
            _ => None,
        }
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
            "role": "Habits and diary",
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
        assert_eq!(manifest().role.as_deref(), Some("Habits and diary"));
    }

    #[test]
    fn a_carried_moon_names_its_planet() {
        let mut m = manifest();
        m.parent = Some("neptune".into());
        m.app = Some("neptune".into());
        assert!(m.problems().is_empty(), "{:?}", m.problems());
        m.app = Some("terra".into());
        assert_eq!(m.problems().len(), 1);
        let back: WorldManifest =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back.app.as_deref(), Some("terra"));
    }

    #[test]
    fn problems_are_spelled_out() {
        let mut m = manifest();
        m.id = "Sol!".into();
        m.repo = "nope".into();
        m.emits[0].kind = "neptune.track.played".into();
        m.settings[0].default = Some(json!("25:00"));
        assert_eq!(m.problems().len(), 4, "{:?}", m.problems());
        m.settings[0].shared = Some("Navidrome URL".into());
        assert_eq!(m.problems().len(), 5, "{:?}", m.problems());
    }

    #[test]
    fn kinds_from_newer_worlds_still_parse() {
        let mut json = serde_json::to_value(manifest()).unwrap();
        json["settings"][0]["kind"] = json!("colour");
        json["settings"][0]["default"] = json!("#ff8800");
        json["actions"][0]["params"][0]["kind"] = json!("date");
        json["something_new"] = json!({ "added": "later" });
        let m: WorldManifest = serde_json::from_value(json).unwrap();
        assert_eq!(m.settings[0].kind, FieldKind::Unknown);
        assert_eq!(m.actions[0].params[0].kind, FieldKind::Unknown);
        assert!(m.problems().is_empty(), "{:?}", m.problems());
        assert!(FieldKind::Unknown.check(&json!("#ff8800")).is_err());
    }

    #[test]
    fn worlds_say_which_sol_they_need() {
        let mut m = manifest();
        assert_eq!(m.unfit_for("1.0.0", 2, 2), None);
        m.min_sol = Some("1.2.0".into());
        m.protocol = Some(2);
        assert!(m.unfit_for("1.2.0", 2, 2).is_none());
        assert!(
            m.unfit_for("1.1.9", 2, 2)
                .unwrap()
                .contains("needs Sol 1.2.0")
        );
        m.min_sol = None;
        m.protocol = Some(3);
        assert!(m.unfit_for("1.0.0", 2, 2).unwrap().contains("protocol 3"));
        m.protocol = Some(1);
        assert!(m.unfit_for("1.0.0", 2, 2).unwrap().contains("no longer"));
        m.min_sol = Some("soon".into());
        assert_eq!(m.problems().len(), 1, "{:?}", m.problems());
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
