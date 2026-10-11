//! For tests, here and in every world app (feature `testing`, which a world
//! turns on for its tests only, through `sol_quick`'s `testing`):
//!
//! - [`offline`] / [`open`]: a [`Link`] with a local copy in a fresh folder
//!   and no Sol, as an app that was never paired.
//! - [`FakeHttp`]: a small HTTP server on 127.0.0.1 that answers each
//!   [`Request`] with a canned [`Reply`] (by method and path, or from a
//!   function) and keeps every request it was sent, for a stand-in AniList,
//!   Ollama or Jimaku.
//! - [`StandInSol`]: a [`FakeHttp`] that answers as Sol does where a test
//!   doesn't care (no settings, long polls held), and [`StandInSol::paired`]
//!   for a link paired with it.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    device::Person,
    link::{Config, Link, Tokens},
};

/// A link with a local copy in a fresh folder and no Sol, as `world`. Keep
/// the folder for as long as the link.
pub async fn offline(world: &str) -> (Link, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a folder for the local copy");
    let link = open(world, dir.path()).await;
    (link, dir)
}

/// A link on the local copy in `dir`, as the app opens it at start (again,
/// for a test of what survives a restart).
pub async fn open(world: &str, dir: &Path) -> Link {
    Link::open(Config {
        world: world.into(),
        dir: dir.to_owned(),
        device: "Test".into(),
        platform: None,
        version: "0.1.0".into(),
        tokens: Tokens::File,
    })
    .await
    .expect("the local copy opens")
}

/// The person a stand-in Sol pairs as: Lukas, in Copenhagen, with id `id`.
pub fn person(id: &str) -> Person {
    Person {
        id: id.into(),
        username: "lukas".into(),
        tz: "Europe/Copenhagen".into(),
    }
}

/// One request a [`FakeHttp`] was sent.
#[derive(Clone, Debug)]
pub struct Request {
    /// `GET`, `PUT`.
    pub method: String,
    /// The target as sent, with its query: `/api/entries/search?anilist_id=7`.
    pub path: String,
    /// Names in lowercase.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// When it arrived.
    pub at: Instant,
}

impl Request {
    /// The path without its query.
    pub fn route(&self) -> &str {
        self.path.split('?').next().unwrap_or_default()
    }

    /// `PUT /api/v1/docs/habits/walk`: the method and the path, with its query.
    pub fn line(&self) -> String {
        format!("{} {}", self.method, self.path)
    }

    /// A header's value, by name in any case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON (null when it isn't).
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }
}

/// What a [`FakeHttp`] answers: a status, headers and a body, JSON unless a
/// header says otherwise.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    /// `status` with `body`, as JSON.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into(),
        }
    }

    /// `200` with `body`, as JSON.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self::new(200, body)
    }

    /// A value as JSON, with `status`.
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::new(status, value.to_string())
    }

    /// The same, with a header more (`retry-after: 2`).
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_lowercase(), value.to_owned()));
        self
    }
}

/// Every request a [`FakeHttp`] was sent, in order; clones share them.
#[derive(Clone, Default)]
pub struct Requests(Arc<Mutex<Vec<Request>>>);

impl Requests {
    /// All of them so far.
    pub fn all(&self) -> Vec<Request> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Those with `method` (`PUT`).
    pub fn with_method(&self, method: &str) -> Vec<Request> {
        self.all()
            .into_iter()
            .filter(|r| r.method == method)
            .collect()
    }

    fn push(&self, request: Request) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(request);
    }
}

/// How a [`FakeHttp`] answers a request, given every request so far (this
/// one last): a reply, or None to hold it open without one (a long poll).
pub type Answer = dyn Fn(&Request, &Requests) -> Option<Reply> + Send + Sync;

/// A small HTTP server on 127.0.0.1 for tests: one thread, a thread per
/// request, every connection closed after its reply.
pub struct FakeHttp {
    url: String,
    requests: Requests,
}

impl FakeHttp {
    /// Answers each request with `answer`.
    pub fn start(
        answer: impl Fn(&Request, &Requests) -> Option<Reply> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let requests = Requests::default();
        let seen = requests.clone();
        let answer: Arc<Answer> = Arc::new(answer);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (seen, answer) = (seen.clone(), answer.clone());
                std::thread::spawn(move || serve(stream, &seen, &*answer));
            }
        });
        Self { url, requests }
    }

    /// Answers `"GET /api/tags"` (the method and the path without its
    /// query) with its reply, and anything else with `404`.
    pub fn routes(routes: impl IntoIterator<Item = (&'static str, Reply)>) -> Self {
        let routes: Vec<(&str, Reply)> = routes.into_iter().collect();
        Self::start(move |request, _| {
            let asked = format!("{} {}", request.method, request.route());
            Some(
                routes
                    .iter()
                    .find(|(route, _)| *route == asked)
                    .map_or_else(|| Reply::new(404, "{}"), |(_, reply)| reply.clone()),
            )
        })
    }

    /// `http://127.0.0.1:<port>`, without a slash at the end.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every request so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.all()
    }

    /// The same, to keep and read later.
    pub fn seen(&self) -> Requests {
        self.requests.clone()
    }
}

/// Reads one request, keeps it, and answers it.
fn serve(stream: TcpStream, seen: &Requests, answer: &Answer) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        return;
    }
    let mut words = first.split_whitespace();
    let (method, path) = (
        words.next().unwrap_or_default().to_owned(),
        words.next().unwrap_or("/").to_owned(),
    );
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let request = Request {
        method,
        path,
        headers,
        body,
        at: Instant::now(),
    };
    seen.push(request.clone());
    let Some(reply) = answer(&request, seen) else {
        // Held, as a long poll Sol has nothing new for.
        std::thread::sleep(Duration::from_secs(30));
        return;
    };
    let mut head = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        reply.body.len()
    ));
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(&reply.body);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Reply",
    }
}

/// A stand-in Sol: no settings set, long polls (changes, the inbox) held,
/// and every other request handed to a function.
pub struct StandInSol {
    http: FakeHttp,
}

impl StandInSol {
    /// Hands document writes and the rest to `answer` (None holds one).
    pub fn start(
        answer: impl Fn(&Request, &Requests) -> Option<Reply> + Send + Sync + 'static,
    ) -> Self {
        let http = FakeHttp::start(move |request, seen| {
            if request.route().ends_with("/api/v1/settings") {
                Some(Reply::ok("{}"))
            } else {
                answer(request, seen)
            }
        });
        Self { http }
    }

    /// Answers only settings, and holds everything else.
    pub fn quiet() -> Self {
        Self::start(|_, _| None)
    }

    pub fn url(&self) -> &str {
        self.http.url()
    }

    /// Every request so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.http.requests()
    }

    /// The bodies of the document writes so far, in order.
    pub fn puts(&self) -> Vec<serde_json::Value> {
        puts(&self.http.seen())
    }

    /// A link paired with this Sol as `person` (device `d1`, a token in a
    /// file), not yet syncing: `link.start(…)` sets it going.
    pub async fn paired(&self, world: &str, person: &str) -> (Link, tempfile::TempDir) {
        paired_with(self.url(), world, person).await
    }
}

/// The bodies of the document writes among `seen`, in order.
pub fn puts(seen: &Requests) -> Vec<serde_json::Value> {
    seen.with_method("PUT").iter().map(Request::json).collect()
}

/// A link paired with the Sol at `server` (which need not answer) as
/// `person`, not yet syncing.
pub async fn paired_with(server: &str, world: &str, person: &str) -> (Link, tempfile::TempDir) {
    let (link, dir) = offline(world).await;
    link.pretend_paired(server, "d1", &self::person(person))
        .await
        .expect("the pairing is written down");
    std::fs::write(dir.path().join(format!("{world}-d1.token")), "sol_test")
        .expect("the token is written");
    drop(link);
    let link = open(world, dir.path()).await;
    (link, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fake_server_answers_by_route_and_keeps_what_it_was_asked() {
        let http = FakeHttp::routes([
            ("GET /api/version", Reply::ok(r#"{"version":"1"}"#)),
            (
                "POST /api/embed",
                Reply::new(429, "{}").with_header("retry-after", "2"),
            ),
        ]);
        let client = reqwest::Client::new();
        let got = client
            .get(format!("{}/api/version?x=1", http.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(got.status(), 200);
        assert_eq!(got.text().await.unwrap(), r#"{"version":"1"}"#);
        let busy = client
            .post(format!("{}/api/embed", http.url()))
            .header("authorization", "key")
            .body(r#"{"input":["a"]}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(busy.status(), 429);
        assert_eq!(busy.headers()["retry-after"], "2");
        let missing = client
            .get(format!("{}/nothing", http.url()))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);

        let asked = http.requests();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked[0].line(), "GET /api/version?x=1");
        assert_eq!(asked[0].route(), "/api/version");
        assert_eq!(asked[1].header("Authorization"), Some("key"));
        assert_eq!(asked[1].json()["input"][0], "a");
    }
}
