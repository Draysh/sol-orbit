use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Version of the app contract described in `docs/app-contract.md`.
pub const CONTRACT: u32 = 1;

/// What an app tells Sol about itself at `GET /_sol/manifest`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Manifest {
    pub contract: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Serves a web UI at `/ui/`, which Sol shows at `/<id>/`.
    #[serde(default)]
    pub ui: bool,
    #[serde(default)]
    pub widgets: Vec<Widget>,
    #[serde(default)]
    pub events: Events,
}

/// A dashboard widget: an API route that returns a `WidgetView`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Widget {
    pub id: String,
    pub title: String,
    /// Path under the app's API, e.g. `/widgets/today` (Sol serves it at `/api/<app>/widgets/today`).
    pub path: String,
    /// Event types (`*` allowed at the end) that should refresh it.
    #[serde(default)]
    pub refresh_on: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Events {
    #[serde(default)]
    pub emits: Vec<EventType>,
    #[serde(default)]
    pub consumes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventType {
    #[serde(rename = "type")]
    pub kind: String,
    pub v: u32,
    /// Sol raises a system notification for it while Sol isn't in front.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notify: bool,
}

/// `GET /_sol/health`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub status: String,
    pub version: String,
    /// CPU architecture the binary was built for, e.g. `aarch64`.
    pub arch: String,
    pub uptime_s: u64,
    pub db: String,
}

impl Manifest {
    /// A manifest with the contract version and crate version filled in.
    pub fn new(id: &str, name: &str, version: &str, description: &str) -> Self {
        Self {
            contract: CONTRACT,
            id: id.into(),
            name: name.into(),
            version: version.into(),
            description: description.into(),
            ui: false,
            widgets: Vec::new(),
            events: Events::default(),
        }
    }

    pub fn widget(mut self, id: &str, title: &str, path: &str, refresh_on: &[&str]) -> Self {
        self.widgets.push(Widget {
            id: id.into(),
            title: title.into(),
            path: path.into(),
            refresh_on: refresh_on.iter().map(|s| (*s).into()).collect(),
        });
        self
    }

    pub fn emits(mut self, kind: &str, v: u32) -> Self {
        self.events.emits.push(EventType {
            kind: kind.into(),
            v,
            notify: false,
        });
        self
    }

    /// Like [`Manifest::emits`], and people get a notification for it.
    pub fn notifies(mut self, kind: &str, v: u32) -> Self {
        self.events.emits.push(EventType {
            kind: kind.into(),
            v,
            notify: true,
        });
        self
    }
}
