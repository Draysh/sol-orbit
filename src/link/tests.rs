use serde_json::json;

use super::{
    cache::{cursor, set_meta},
    pairing::normalise,
    *,
};
use crate::Doc;

async fn offline() -> (Link, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let link = Link::open(Config {
        world: "terra".into(),
        dir: dir.path().to_owned(),
        device: "Test".into(),
        platform: None,
        version: "0.1.0".into(),
        tokens: Tokens::File,
    })
    .await
    .unwrap();
    (link, dir)
}

#[tokio::test]
async fn works_offline_and_queues_in_order() {
    let (link, _dir) = offline().await;
    assert_eq!(link.status(), Status::Unpaired { error: None });

    let walk = link
        .put("habits", "walk", json!({ "name": "Walk" }))
        .await
        .unwrap();
    assert_eq!(walk.version, 0, "Sol hasn't seen it yet");
    link.put("habits", "read", json!({ "name": "Read" }))
        .await
        .unwrap();
    link.delete("habits", "read").await.unwrap();
    link.emit(Emit::new("terra.habit.checked", "Did “Walk”"))
        .await
        .unwrap();
    link.push_widget("today", "Today", WidgetView::default())
        .await
        .unwrap();
    link.push_widget("today", "Today", WidgetView::default())
        .await
        .unwrap();

    let names: Vec<_> = link
        .list("habits")
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(names, ["walk"]);
    assert!(link.get("habits", "read").await.unwrap().is_none());
    // Two writes, a delete, an event and one widget (the newer push replaced the older).
    assert_eq!(link.unsent().await.unwrap(), 5);
    assert!(link.put("Habits", "x", json!({})).await.is_err());
}

#[tokio::test]
async fn changes_from_sol_wait_behind_local_writes() {
    let (link, _dir) = offline().await;
    link.put("habits", "walk", json!({ "name": "Mine" }))
        .await
        .unwrap();
    let theirs = |id: &str, version: i64| Doc {
        collection: "habits".into(),
        id: id.into(),
        version,
        updated_at: Utc::now(),
        deleted: false,
        data: json!({ "name": "Theirs" }),
    };
    link.apply(crate::doc::ChangePage {
        changes: vec![theirs("walk", 7), theirs("read", 8)],
        next: 8,
        more: false,
        database: None,
    })
    .await
    .unwrap();
    // The unsent local write wins for now; the other change comes in.
    assert_eq!(
        link.get("habits", "walk").await.unwrap().unwrap().data["name"],
        "Mine"
    );
    assert_eq!(
        link.get("habits", "read").await.unwrap().unwrap().version,
        8
    );
    let next = link
        .0
        .db
        .call(|c| cursor(c, "changes"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next, 8);
}

#[tokio::test]
async fn documents_come_back_as_types() {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Habit {
        name: String,
        weight: f32,
    }
    let (link, _dir) = offline().await;
    let walk = Habit {
        name: "Walk".into(),
        weight: 7.3,
    };
    let written = link.put_as("habits", "walk", &walk).await.unwrap();
    assert_eq!(written.data, json!({ "name": "Walk", "weight": 7.3 }));
    link.put("habits", "odd", json!({ "title": "Not a habit" }))
        .await
        .unwrap();

    let habits: Vec<Habit> = link.list_as("habits").await.unwrap();
    assert_eq!(habits, [walk]);
    let ids: Vec<String> = link
        .list_with_ids::<Habit>("habits")
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, ["walk"]);
    let counted = link.list_counted::<Habit>("habits").await.unwrap();
    assert_eq!(counted.rows.len(), 1);
    assert_eq!(counted.unreadable, 1);
    assert!(
        link.get_as::<Habit>("habits", "walk")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        link.get_as::<Habit>("habits", "odd")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn settings_are_followed_from_the_start() {
    let (link, _dir) = offline().await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let follow = tokio::spawn(link.on_settings(move |why| {
        let tx = tx.clone();
        async move {
            let _ = tx.send(why);
        }
    }));
    assert_eq!(rx.recv().await, Some(Configure::Start));
    link.send(Update::Changed(vec!["habits".into()]));
    link.send(Update::Settings);
    assert_eq!(rx.recv().await, Some(Configure::Settings));
    link.send(Update::Status(Status::Unpaired { error: None }));
    assert_eq!(
        rx.recv().await,
        Some(Configure::Status(Status::Unpaired { error: None }))
    );
    drop(link);
    follow.await.unwrap();
}

#[test]
fn addresses() {
    assert_eq!(
        normalise("sol.local:8080/").unwrap(),
        "http://sol.local:8080"
    );
    assert_eq!(
        normalise(" https://sol.example.ts.net ").unwrap(),
        "https://sol.example.ts.net"
    );
    assert!(normalise("ftp://x").is_err());
}

/// Stands in for Sol: answers the first document write with `429` and
/// `Retry-After: 2`, the next one with the document, and holds long polls.
async fn busy_sol() -> (String, Arc<Mutex<Vec<(std::time::Instant, String)>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, body_len) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        break (head, end + 4 + len);
                    }
                };
                while buf.len() < body_len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let line = head.lines().next().unwrap_or_default().to_owned();
                let writes = {
                    let mut log = log.lock().unwrap();
                    log.push((std::time::Instant::now(), line.clone()));
                    log.iter().filter(|(_, l)| l.starts_with("PUT ")).count()
                };
                let (status, extra, body) = if line.starts_with("PUT ") && writes == 1 {
                    (
                        "429 Too Many Requests",
                        "retry-after: 2\r\n",
                        r#"{"error":"rate_limited","message":"Sol takes more again shortly"}"#
                            .to_owned(),
                    )
                } else if line.starts_with("PUT ") {
                    (
                        "200 OK",
                        "",
                        r#"{"collection":"habits","id":"walk","version":1,"updated_at":"2026-10-10T08:00:00Z","deleted":false,"data":{"name":"Walk"}}"#.to_owned(),
                    )
                } else if line.contains("/api/v1/settings") {
                    ("200 OK", "", "{}".to_owned())
                } else {
                    // Long polls: nothing yet.
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                };
                let res = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(res.as_bytes()).await;
            });
        }
    });
    (base, seen)
}

#[tokio::test]
async fn a_busy_sol_is_given_the_time_it_asks_for() {
    let (base, seen) = busy_sol().await;
    let (link, dir) = offline().await;
    // As if paired with that Sol.
    let user = serde_json::to_string(&Person {
        id: "u1".into(),
        username: "lukas".into(),
        tz: "Europe/Copenhagen".into(),
    })
    .unwrap();
    link.0
        .db
        .call(move |c| -> rusqlite::Result<()> {
            set_meta(c, "server", &base)?;
            set_meta(c, "device", "d1")?;
            set_meta(c, "user", &user)
        })
        .await
        .unwrap()
        .unwrap();
    std::fs::write(dir.path().join("terra-d1.token"), "sol_test").unwrap();
    drop(link);
    let link = Link::open(Config {
        world: "terra".into(),
        dir: dir.path().to_owned(),
        device: "Test".into(),
        platform: None,
        version: "0.1.0".into(),
        tokens: Tokens::File,
    })
    .await
    .unwrap();
    link.put("habits", "walk", json!({ "name": "Walk" }))
        .await
        .unwrap();
    link.start(Arc::new(Ignore));

    let mut status = link.subscribe();
    let told = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Update::Status(Status::Offline { error: Some(e), .. })) = status.recv().await
            {
                break e;
            }
        }
    })
    .await
    .expect("the link says Sol asked it to wait");
    assert!(told.contains("shortly"), "{told}");
    for _ in 0..100 {
        if link.unsent().await.unwrap() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        link.unsent().await.unwrap(),
        0,
        "the write went through in the end"
    );
    let puts: Vec<std::time::Instant> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, l)| l.starts_with("PUT "))
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(puts.len(), 2, "one refused, one taken");
    assert!(
        puts[1] - puts[0] >= Duration::from_millis(1900),
        "waited {:?}, not the 2 s Sol asked for",
        puts[1] - puts[0]
    );
    assert_eq!(
        link.get("habits", "walk").await.unwrap().unwrap().version,
        1
    );
}

#[tokio::test]
async fn a_restored_sol_is_read_again_from_the_start() {
    let (link, _dir) = offline().await;
    let doc = |id: &str, version: i64| Doc {
        collection: "habits".into(),
        id: id.into(),
        version,
        updated_at: Utc::now(),
        deleted: false,
        data: json!({ "name": id }),
    };
    let page = |changes: Vec<Doc>, next, database: &str| crate::doc::ChangePage {
        changes,
        next,
        more: false,
        database: Some(database.into()),
    };
    let ids = async |link: &Link| -> Vec<String> {
        link.list("habits")
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect()
    };

    assert!(
        link.follows("changes", 0, Some("one".into()))
            .await
            .unwrap()
    );
    link.apply(page(vec![doc("walk", 1), doc("read", 2)], 2, "one"))
        .await
        .unwrap();
    link.put("habits", "mine", json!({ "name": "mine" }))
        .await
        .unwrap();
    assert!(
        link.follows("changes", 2, Some("one".into()))
            .await
            .unwrap()
    );
    // An old page, asked for before another loop started over, is dropped.
    assert!(
        !link
            .follows("changes", 1, Some("one".into()))
            .await
            .unwrap()
    );

    // Sol comes back from a copy that has only "walk": a new id.
    assert!(
        !link
            .follows("changes", 2, Some("two".into()))
            .await
            .unwrap()
    );
    let cursors = link
        .0
        .db
        .call(|c| (cursor(c, "changes").unwrap(), cursor(c, "inbox").unwrap()))
        .await
        .unwrap();
    assert_eq!(cursors, (0, 0));
    // Everything stays on screen until the full read is in.
    assert_eq!(ids(&link).await, ["mine", "read", "walk"]);
    assert!(
        link.follows("changes", 0, Some("two".into()))
            .await
            .unwrap()
    );
    link.apply(page(vec![doc("walk", 1)], 1, "two"))
        .await
        .unwrap();
    assert_eq!(
        ids(&link).await,
        ["mine", "walk"],
        "the write waiting for Sol stays"
    );
    assert_eq!(link.unsent().await.unwrap(), 1);
    assert_eq!(
        link.get("habits", "walk").await.unwrap().unwrap().version,
        1
    );
}

// ---- Writes are kept, never lost ---------------------------------------------

/// One request a stand-in Sol saw: its first line and its body.
type Seen = Arc<Mutex<Vec<(String, String)>>>;
/// How a stand-in Sol answers a request (`None`: holds it, as a long poll).
type Answer = Arc<dyn Fn(&str, &str, &Seen) -> Option<(u16, String)> + Send + Sync>;

/// A stand-in Sol that answers each request with `answer`.
async fn stand_in(answer: Answer) -> (String, Seen) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (log, answer) = (log.clone(), answer.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, start, len) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        break (head, end + 4, len);
                    }
                };
                while buf.len() < start + len {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let line = head.lines().next().unwrap_or_default().to_owned();
                let body = String::from_utf8_lossy(&buf[start..]).into_owned();
                log.lock().unwrap().push((line.clone(), body.clone()));
                let Some((status, reply)) = answer(&line, &body, &log) else {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                };
                let res = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = sock.write_all(res.as_bytes()).await;
            });
        }
    });
    (base, seen)
}

fn lukas(id: &str) -> Person {
    Person {
        id: id.into(),
        username: "lukas".into(),
        tz: "Europe/Copenhagen".into(),
    }
}

/// A link that is paired with the Sol at `base` as `person`, not yet syncing.
async fn paired_with(base: &str, person: &str) -> (Link, tempfile::TempDir) {
    let (link, dir) = offline().await;
    let (base, user) = (
        base.to_owned(),
        serde_json::to_string(&lukas(person)).unwrap(),
    );
    link.0
        .db
        .call(move |c| -> rusqlite::Result<()> {
            set_meta(c, "server", &base)?;
            set_meta(c, "device", "d1")?;
            set_meta(c, "user", &user)
        })
        .await
        .unwrap()
        .unwrap();
    std::fs::write(dir.path().join("terra-d1.token"), "sol_test").unwrap();
    drop(link);
    let link = Link::open(Config {
        world: "terra".into(),
        dir: dir.path().to_owned(),
        device: "Test".into(),
        platform: None,
        version: "0.1.0".into(),
        tokens: Tokens::File,
    })
    .await
    .unwrap();
    (link, dir)
}

async fn until(what: &str, mut ok: impl AsyncFnMut() -> bool) {
    for _ in 0..200 {
        if ok().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn doc_json(id: &str, version: i64, name: &str) -> String {
    format!(
        r#"{{"collection":"habits","id":"{id}","version":{version},"updated_at":"2026-10-10T08:00:00Z","deleted":false,"data":{{"name":"{name}"}}}}"#
    )
}

/// Answers settings, holds long polls, and hands document writes to `put`.
fn sol_with(put: impl Fn(&str, &Seen) -> (u16, String) + Send + Sync + 'static) -> Answer {
    Arc::new(move |line, body, seen| {
        if line.starts_with("PUT ") {
            Some(put(body, seen))
        } else if line.contains("/api/v1/settings") {
            Some((200, "{}".into()))
        } else {
            None
        }
    })
}

fn puts(seen: &Seen) -> Vec<serde_json::Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(l, _)| l.starts_with("PUT "))
        .map(|(_, b)| serde_json::from_str(b).unwrap())
        .collect()
}

#[tokio::test]
async fn unpairing_keeps_waiting_writes_for_the_same_person() {
    let (link, _dir) = paired_with("http://127.0.0.1:9", "u1").await;
    link.put("habits", "walk", json!({ "name": "Walk" }))
        .await
        .unwrap();
    link.delete("habits", "walk").await.unwrap();
    link.put("habits", "read", json!({ "name": "Read" }))
        .await
        .unwrap();
    link.emit(Emit::new("terra.habit.checked", "Did “Read”"))
        .await
        .unwrap();

    // Sol said 401 (unpaired there), or the person unpaired: nothing is lost.
    link.reset(Some("Sol unpaired this device".into()))
        .await
        .unwrap();
    assert_eq!(link.unsent().await.unwrap(), 0);
    assert!(link.list("habits").await.unwrap().is_empty());
    assert_eq!(
        link.kept().await.unwrap(),
        Kept {
            earlier: 4,
            ..Kept::default()
        }
    );

    // Paired again as the same person: everything waits to go, in order,
    // and shows at once.
    let paired = |who: &str| crate::device::Paired {
        token: "sol_again".into(),
        device_id: "d2".into(),
        world: "terra".into(),
        user: lukas(who),
    };
    link.finish("http://127.0.0.1:9", paired("u1"))
        .await
        .unwrap();
    assert!(link.kept().await.unwrap().is_empty());
    assert_eq!(link.unsent().await.unwrap(), 4);
    let kinds: Vec<String> = link
        .0
        .db
        .call(|c| {
            c.prepare("SELECT kind FROM pending ORDER BY seq")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kinds, ["put", "delete", "put", "event"]);
    assert_eq!(
        link.get("habits", "read").await.unwrap().unwrap().data["name"],
        "Read"
    );
    assert!(link.get("habits", "walk").await.unwrap().is_none());

    // Unpaired again, and paired by someone else: kept, to be seen and saved.
    link.reset(None).await.unwrap();
    link.finish("http://127.0.0.1:9", paired("u2"))
        .await
        .unwrap();
    assert_eq!(link.unsent().await.unwrap(), 0);
    assert_eq!(link.kept().await.unwrap().earlier, 4);
    let export = link.export_kept().await.unwrap();
    assert_eq!(export["world"], "terra");
    let writes = export["writes"].as_array().unwrap();
    assert_eq!(writes.len(), 4);
    assert_eq!(writes[2]["data"]["name"], "Read");
    assert_eq!(writes[0]["why"], "earlier");
    for w in link.kept_writes().await.unwrap() {
        link.drop_kept(w.seq).await.unwrap();
    }
    assert!(link.kept().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_401_from_sol_sets_the_writes_aside() {
    let (base, _) = stand_in(sol_with(|_, _| {
        (
            401,
            r#"{"error":"unauthorized","message":"this device was unpaired"}"#.into(),
        )
    }))
    .await;
    let (link, _dir) = paired_with(&base, "u1").await;
    link.put("habits", "walk", json!({ "name": "Walk" }))
        .await
        .unwrap();
    link.start(Arc::new(Ignore));
    until("the link to notice", async || {
        matches!(link.status(), Status::Unpaired { .. })
    })
    .await;
    assert_eq!(link.kept().await.unwrap().earlier, 1);
    assert_eq!(link.unsent().await.unwrap(), 0);
}

#[tokio::test]
async fn writes_build_on_the_version_they_were_made_on() {
    let (base, seen) = stand_in(sol_with(|_, seen| {
        let n = puts(seen).len() as i64;
        (200, doc_json("walk", 10 + n, "Walk"))
    }))
    .await;
    let (link, _dir) = paired_with(&base, "u1").await;
    // Sol's copy is at version 7 when the two writes are made.
    link.apply(crate::doc::ChangePage {
        changes: vec![serde_json::from_str(&doc_json("walk", 7, "Theirs")).unwrap()],
        next: 7,
        more: false,
        database: None,
    })
    .await
    .unwrap();
    link.put("habits", "walk", json!({ "name": "One" }))
        .await
        .unwrap();
    link.put("habits", "walk", json!({ "name": "Two" }))
        .await
        .unwrap();
    link.put("habits", "new", json!({ "name": "New" }))
        .await
        .unwrap();
    link.overwrite("habits", "state", json!({ "name": "Now" }))
        .await
        .unwrap();
    link.start(Arc::new(Ignore));
    until("everything to go", async || {
        link.unsent().await.unwrap() == 0
    })
    .await;
    let sent = puts(&seen);
    assert_eq!(sent[0]["if_version"], 7);
    // The second builds on the first one's new version (11), not on 7.
    assert_eq!(sent[1]["if_version"], 11);
    // A document Sol hasn't seen: only if nobody made it meanwhile.
    assert_eq!(sent[2]["if_version"], 0);
    // An overwrite goes in whatever is there.
    assert!(sent[3].get("if_version").is_none_or(|v| v.is_null()));
    assert!(link.kept().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_conflict_keeps_both_copies() {
    // The first write meets another device's newer copy; later ones go in.
    let (base, seen) = stand_in(sol_with(|body, seen| {
        let n = puts(seen).len();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        if n == 1 {
            (
                409,
                format!(
                    r#"{{"error":"conflict","message":"the document changed in the meantime","current":{}}}"#,
                    doc_json("walk", 9, "Theirs")
                ),
            )
        } else {
            let name = body["data"]["name"].as_str().unwrap().to_owned();
            (200, doc_json("walk", 10, &name))
        }
    }))
    .await;
    let (link, _dir) = paired_with(&base, "u1").await;
    link.apply(crate::doc::ChangePage {
        changes: vec![serde_json::from_str(&doc_json("walk", 7, "Before")).unwrap()],
        next: 7,
        more: false,
        database: None,
    })
    .await
    .unwrap();
    let mut updates = link.subscribe();
    link.put("habits", "walk", json!({ "name": "Mine" }))
        .await
        .unwrap();
    link.start(Arc::new(Ignore));
    until("the conflict", async || {
        link.kept().await.unwrap().conflicts == 1
    })
    .await;
    // Sol's copy is shown; this device's is kept beside it.
    assert_eq!(
        link.get("habits", "walk").await.unwrap().unwrap().data["name"],
        "Theirs"
    );
    assert_eq!(link.unsent().await.unwrap(), 0);
    let kept = link.kept_writes().await.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].why, Why::Conflict);
    assert_eq!(kept[0].data["name"], "Mine");
    assert_eq!(kept[0].theirs.as_ref().unwrap().version, 9);
    // The screens were told.
    let told = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(Update::Kept(k)) = updates.recv().await
                && k.conflicts == 1
            {
                break;
            }
        }
    })
    .await;
    assert!(told.is_ok(), "no Update::Kept");

    // The person picks this device's copy: it goes over Sol's version 9.
    link.keep_mine(kept[0].seq).await.unwrap();
    assert_eq!(
        link.get("habits", "walk").await.unwrap().unwrap().data["name"],
        "Mine"
    );
    until("the chosen copy to go", async || {
        link.unsent().await.unwrap() == 0 && puts(&seen).len() == 2
    })
    .await;
    assert_eq!(puts(&seen)[1]["if_version"], 9);
    assert!(link.kept().await.unwrap().is_empty());
    assert!(
        link.keep_mine(kept[0].seq).await.is_err(),
        "settled already"
    );
}

#[tokio::test]
async fn a_lost_answer_is_not_a_conflict() {
    // Sol took the write, but its answer never came: sent again, it meets
    // its own copy, which says the same.
    let (base, _) = stand_in(sol_with(|_, _| {
        (
            409,
            format!(
                r#"{{"error":"conflict","message":"changed","current":{}}}"#,
                doc_json("walk", 8, "Mine")
            ),
        )
    }))
    .await;
    let (link, _dir) = paired_with(&base, "u1").await;
    link.put("habits", "walk", json!({ "name": "Mine" }))
        .await
        .unwrap();
    link.start(Arc::new(Ignore));
    until("the write to settle", async || {
        link.unsent().await.unwrap() == 0
    })
    .await;
    assert!(link.kept().await.unwrap().is_empty());
    assert_eq!(
        link.get("habits", "walk").await.unwrap().unwrap().version,
        8
    );
}

#[tokio::test]
async fn a_rejected_write_is_kept_and_the_rest_go() {
    let (base, _) = stand_in(sol_with(|_, seen| {
        if puts(seen).len() == 1 {
            (
                413,
                r#"{"error":"too_large","message":"a document is at most 1 MiB"}"#.into(),
            )
        } else {
            (200, doc_json("read", 3, "Read"))
        }
    }))
    .await;
    let (link, _dir) = paired_with(&base, "u1").await;
    link.put("habits", "walk", json!({ "name": "Huge" }))
        .await
        .unwrap();
    link.put("habits", "read", json!({ "name": "Read" }))
        .await
        .unwrap();
    link.start(Arc::new(Ignore));
    until("both to settle", async || link.unsent().await.unwrap() == 0).await;
    let kept = link.kept_writes().await.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].why, Why::Rejected);
    assert_eq!(
        kept[0].message.as_deref(),
        Some("a document is at most 1 MiB")
    );
    assert_eq!(kept[0].data["name"], "Huge");
    assert_eq!(
        link.get("habits", "read").await.unwrap().unwrap().version,
        3
    );
}

#[tokio::test]
async fn a_copy_that_wont_open_says_why_and_can_be_set_aside() {
    let open = async |dir: &std::path::Path| {
        Link::open(Config {
            world: "terra".into(),
            dir: dir.to_owned(),
            device: "Test".into(),
            platform: None,
            version: "0.1.0".into(),
            tokens: Tokens::File,
        })
        .await
    };
    // Written by a newer version, with a migration this one doesn't know.
    let (link, dir) = offline().await;
    link.put("habits", "walk", json!({ "name": "Walk" }))
        .await
        .unwrap();
    drop(link);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let db = dir.path().join("terra.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    let err = open(dir.path()).await.err().expect("a newer copy");
    let problem = err
        .downcast_ref::<LocalCopyError>()
        .expect("a LocalCopyError");
    assert_eq!(problem.problem, LocalCopyProblem::Newer);
    assert_eq!(problem.path, db);

    // Damaged: not a database at all.
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("terra.db"), vec![7u8; 8192]).unwrap();
    let err = open(broken.path()).await.err().expect("a damaged copy");
    assert_eq!(
        err.downcast_ref::<LocalCopyError>().unwrap().problem,
        LocalCopyProblem::Damaged
    );

    // Set aside, the app starts from nothing; the old file is still there.
    let aside = set_aside(dir.path(), "terra").unwrap();
    assert!(aside.exists() && !db.exists());
    assert!(
        open(dir.path())
            .await
            .unwrap()
            .list("habits")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_older_copy_is_backed_up_before_it_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("terra.db");
    {
        let mut c = rusqlite::Connection::open(&db).unwrap();
        rusqlite_migration::Migrations::from_slice(&MIGRATIONS[..1])
            .to_latest(&mut c)
            .unwrap();
        c.execute(
            "INSERT INTO pending (kind, collection, id, body) VALUES ('put', 'habits', 'walk', '{}')",
            [],
        )
        .unwrap();
    }
    let link = Link::open(Config {
        world: "terra".into(),
        dir: dir.path().to_owned(),
        device: "Test".into(),
        platform: None,
        version: "0.1.0".into(),
        tokens: Tokens::File,
    })
    .await
    .unwrap();
    // The old waiting write is still there, with no version to build on.
    assert_eq!(link.unsent().await.unwrap(), 1);
    let copies = std::fs::read_dir(dir.path().join("backups"))
        .unwrap()
        .count();
    assert_eq!(copies, 1);
}

#[tokio::test(start_paused = true)]
async fn a_loop_that_panics_starts_again() {
    let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counted = runs.clone();
    super::sync::keep_alive_for_tests(move || {
        let n = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            if n == 0 {
                panic!("a bug");
            }
        }
    })
    .await;
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
}
