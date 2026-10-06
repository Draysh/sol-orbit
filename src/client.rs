//! The client a world's app uses to reach Sol.
//!
//! ```no_run
//! # async fn run() -> Result<(), orbit::client::Error> {
//! use std::time::Duration;
//! use orbit::client::Sol;
//!
//! // Once: pair, show the code, wait for the person to approve it in Sol.
//! let sol = Sol::new("https://sol.example.ts.net");
//! let started = sol.pair("terra", "Desktop", Some("linux")).await?;
//! println!("Approve {} in Sol under Devices", started.code);
//! let paired = loop {
//!     if let Some(paired) = sol.claim(&started, Duration::from_secs(25)).await? {
//!         break paired;
//!     }
//! };
//!
//! // From then on, with the token (keep it in the system keyring).
//! let sol = Sol::new("https://sol.example.ts.net").with_token(paired.token);
//! sol.put("habits", "walk", serde_json::json!({ "name": "Walk outside" }), None).await?;
//! let page = sol.changes(0, Duration::ZERO).await?;
//! # Ok(()) }
//! ```

use std::{collections::BTreeMap, time::Duration};

use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;

use crate::{
    Doc, ErrorBody, WidgetView,
    device::{DeviceMe, InboxPage, PairClaim, PairRequest, PairStarted, Paired, WidgetPush},
    doc::{ChangePage, Conflict, Put},
    event::{Emit, Envelope},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Sol answered with an error; `code` is stable, `message` is for people.
    #[error("{message} ({status} {code})")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    /// `if_version` didn't match; here is the document as it is now.
    #[error("the document changed in the meantime")]
    Conflict { current: Option<Box<Doc>> },
    /// The pairing request expired or was declined; start a new one.
    #[error("the pairing request expired or was declined")]
    PairingGone,
    #[error("couldn't reach Sol: {0}")]
    Transport(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A connection to one Sol, as one world.
#[derive(Clone)]
pub struct Sol {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Sol {
    /// `base` is the address people open Sol at, e.g. `https://sol.example.ts.net`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
            token: None,
            http: reqwest::Client::builder()
                .user_agent(concat!("orbit/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("a client with no custom TLS always builds"),
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Starts pairing; show `code` to the person.
    pub async fn pair(
        &self,
        world: &str,
        device: &str,
        platform: Option<&str>,
    ) -> Result<PairStarted> {
        let body = PairRequest {
            world: world.into(),
            device: device.into(),
            platform: platform.map(Into::into),
        };
        json(self.req(Method::POST, "/api/v1/pair").json(&body)).await
    }

    /// Asks whether the person approved the pairing yet, waiting up to `wait`
    /// (Sol caps it at 25 s). `None` while it is still pending.
    pub async fn claim(&self, started: &PairStarted, wait: Duration) -> Result<Option<Paired>> {
        let body = PairClaim {
            id: started.id.clone(),
            secret: started.secret.clone(),
        };
        let res = self
            .req(
                Method::POST,
                &format!("/api/v1/pair/claim?wait={}", wait.as_secs()),
            )
            .json(&body)
            .timeout(wait + Duration::from_secs(10))
            .send()
            .await?;
        match res.status() {
            StatusCode::ACCEPTED => Ok(None),
            StatusCode::GONE => Err(Error::PairingGone),
            _ => Ok(Some(decode(res).await?)),
        }
    }

    pub async fn me(&self) -> Result<DeviceMe> {
        json(self.req(Method::GET, "/api/v1/me")).await
    }

    /// Unpairs this device; its token stops working.
    pub async fn unpair(&self) -> Result<()> {
        let res = self.req(Method::DELETE, "/api/v1/me").send().await?;
        check(res).await.map(drop)
    }

    /// Whether `base` is a Sol, from its health check.
    pub async fn probe(&self) -> Result<bool> {
        let res = self.req(Method::GET, "/_sol/health").send().await?;
        let body: serde_json::Value = check(res).await?.json().await?;
        Ok(body["status"] == "ok" && body.get("db").is_some())
    }

    /// The address this client talks to.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The world's settings as set in Sol, secrets included, defaults filled in.
    pub async fn settings(&self) -> Result<BTreeMap<String, serde_json::Value>> {
        json(self.req(Method::GET, "/api/v1/settings")).await
    }

    pub async fn list(&self, collection: &str) -> Result<Vec<Doc>> {
        json(self.req(Method::GET, &format!("/api/v1/docs/{collection}"))).await
    }

    pub async fn get(&self, collection: &str, id: &str) -> Result<Option<Doc>> {
        match json(self.req(Method::GET, &format!("/api/v1/docs/{collection}/{id}"))).await {
            Err(Error::Api { status: 404, .. }) => Ok(None),
            other => other.map(Some),
        }
    }

    /// Writes a document. With `if_version`, only if nobody changed it since
    /// (`Some(0)`: only if it doesn't exist yet); otherwise [`Error::Conflict`].
    pub async fn put(
        &self,
        collection: &str,
        id: &str,
        data: serde_json::Value,
        if_version: Option<i64>,
    ) -> Result<Doc> {
        let body = Put { data, if_version };
        json(
            self.req(Method::PUT, &format!("/api/v1/docs/{collection}/{id}"))
                .json(&body),
        )
        .await
    }

    /// Deletes a document; the returned tombstone carries its new version.
    pub async fn delete(&self, collection: &str, id: &str) -> Result<Doc> {
        json(self.req(Method::DELETE, &format!("/api/v1/docs/{collection}/{id}"))).await
    }

    /// Every write after version `after`, waiting up to `wait` for one.
    pub async fn changes(&self, after: i64, wait: Duration) -> Result<ChangePage> {
        json(
            self.req(
                Method::GET,
                &format!("/api/v1/changes?after={after}&wait={}", wait.as_secs()),
            )
            .timeout(wait + Duration::from_secs(10)),
        )
        .await
    }

    /// Posts an event; connections listening for its type run in Sol.
    pub async fn emit(&self, event: &Emit) -> Result<Envelope> {
        json(self.req(Method::POST, "/api/v1/events").json(event)).await
    }

    /// Things other worlds asked this one to do, after cursor `after`.
    pub async fn inbox(&self, after: i64, wait: Duration) -> Result<InboxPage> {
        json(
            self.req(
                Method::GET,
                &format!("/api/v1/inbox?after={after}&wait={}", wait.as_secs()),
            )
            .timeout(wait + Duration::from_secs(10)),
        )
        .await
    }

    /// Claims a delivery for this device before acting on it. `false` means
    /// another of the person's devices already took it.
    pub async fn claim_delivery(&self, n: i64) -> Result<bool> {
        let res = self
            .req(Method::POST, &format!("/api/v1/inbox/{n}/claim"))
            .send()
            .await?;
        match check(res).await {
            Ok(_) => Ok(true),
            Err(Error::Conflict { .. }) => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Replaces one of the world's widgets on Sol's dashboard.
    pub async fn push_widget(&self, id: &str, title: &str, view: WidgetView) -> Result<()> {
        let body = WidgetPush {
            title: title.into(),
            view,
        };
        let res = self
            .req(Method::PUT, &format!("/api/v1/widgets/{id}"))
            .json(&body)
            .send()
            .await?;
        check(res).await.map(drop)
    }

    /// Takes one of the world's widgets off Sol's dashboard.
    pub async fn remove_widget(&self, id: &str) -> Result<()> {
        let res = self
            .req(Method::DELETE, &format!("/api/v1/widgets/{id}"))
            .send()
            .await?;
        check(res).await.map(drop)
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        let req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .timeout(Duration::from_secs(30));
        match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }
}

async fn json<T: DeserializeOwned>(req: RequestBuilder) -> Result<T> {
    decode(req.send().await?).await
}

async fn decode<T: DeserializeOwned>(res: Response) -> Result<T> {
    Ok(check(res).await?.json().await?)
}

async fn check(res: Response) -> Result<Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    if status == StatusCode::CONFLICT {
        let body: Option<Conflict> = res.json().await.ok();
        return Err(Error::Conflict {
            current: body.and_then(|b| b.current).map(Box::new),
        });
    }
    let body: Option<ErrorBody> = res.json().await.ok();
    let (code, message) = body.map_or_else(
        || ("http".to_owned(), status.to_string()),
        |b| (b.error, b.message),
    );
    Err(Error::Api {
        status: status.as_u16(),
        code,
        message,
    })
}
