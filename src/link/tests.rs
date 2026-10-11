use serde_json::json;

use super::{cache::cursor, pairing::normalise, *};
use crate::{
    Doc,
    testing::{Reply, Requests, StandInSol, offline, puts},
};

#[tokio::test]
async fn works_offline_and_queues_in_order() {
    let (link, _dir) = offline("terra").await;
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
    let (link, _dir) = offline("terra").await;
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
    let (link, _dir) = offline("terra").await;
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
async fn documents_are_found_and_counted_by_the_start_of_their_id() {
    let (link, _dir) = offline("saturn").await;
    for id in [
        "anilist-1_s1e1",
        "anilist-1_s1e2",
        "anilist-1_r1_s1e1",
        "anilist-12_s1e1",
        "anilist-2_s1e1",
    ] {
        link.put("watched", id, json!({ "id": id })).await.unwrap();
    }
    link.put("titles", "anilist-1_s1e9", json!({}))
        .await
        .unwrap();
    link.delete("watched", "anilist-1_s1e2").await.unwrap();

    let ids = |docs: Vec<Doc>| docs.into_iter().map(|d| d.id).collect::<Vec<_>>();
    assert_eq!(
        ids(link.list_prefix("watched", "anilist-1_").await.unwrap()),
        ["anilist-1_r1_s1e1", "anilist-1_s1e1"],
        "only that title's, without the deleted one or another collection's"
    );
    assert_eq!(
        ids(link.list_prefix("watched", "anilist-1_s").await.unwrap()),
        ["anilist-1_s1e1"]
    );
    assert_eq!(link.count("watched", "anilist-1_").await.unwrap(), 2);
    assert_eq!(link.count("watched", "anilist-").await.unwrap(), 4);
    assert_eq!(link.count("watched", "").await.unwrap(), 4);
    assert_eq!(link.count("watched", "tmdb-").await.unwrap(), 0);
    let typed: Vec<(String, serde_json::Value)> =
        link.list_prefix_as("watched", "anilist-2").await.unwrap();
    assert_eq!(typed.len(), 1);

    // Both go by the index, never through the whole table.
    let plan = link
        .0
        .db
        .call(|c| {
            c.query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM docs
                 WHERE collection = 'watched' AND id >= 'a' AND id < 'b' AND deleted = 0",
                [],
                |r| r.get::<_, String>(3),
            )
        })
        .await
        .unwrap()
        .unwrap();
    assert!(
        plan.contains("USING INDEX") && plan.contains("id>?"),
        "{plan}"
    );
}

#[tokio::test]
async fn settings_are_followed_from_the_start() {
    let (link, _dir) = offline("terra").await;
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
fn busy_sol() -> StandInSol {
    StandInSol::start(|request, seen| {
        if request.method != "PUT" {
            return None;
        }
        Some(if seen.with_method("PUT").len() == 1 {
            Reply::new(
                429,
                r#"{"error":"rate_limited","message":"Sol takes more again shortly"}"#,
            )
            .with_header("retry-after", "2")
        } else {
            Reply::ok(doc_json("walk", 1, "Walk"))
        })
    })
}

#[tokio::test]
async fn a_busy_sol_is_given_the_time_it_asks_for() {
    let sol = busy_sol();
    let (link, _dir) = sol.paired("terra", "u1").await;
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
    let puts: Vec<std::time::Instant> = sol
        .requests()
        .iter()
        .filter(|r| r.method == "PUT")
        .map(|r| r.at)
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
    let (link, _dir) = offline("terra").await;
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

/// A stand-in Sol that hands each document write (its body, and every
/// request so far) to `put`, and holds the long polls.
fn sol_with(
    put: impl Fn(&serde_json::Value, &Requests) -> (u16, String) + Send + Sync + 'static,
) -> StandInSol {
    StandInSol::start(move |request, seen| {
        (request.method == "PUT").then(|| {
            let (status, body) = put(&request.json(), seen);
            Reply::new(status, body)
        })
    })
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

#[tokio::test]
async fn unpairing_keeps_waiting_writes_for_the_same_person() {
    let (link, _dir) = crate::testing::paired_with("http://127.0.0.1:9", "terra", "u1").await;
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
        user: crate::testing::person(who),
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
    let sol = sol_with(|_, _| {
        (
            401,
            r#"{"error":"unauthorized","message":"this device was unpaired"}"#.into(),
        )
    });
    let (link, _dir) = sol.paired("terra", "u1").await;
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
    let sol = sol_with(|_, seen| {
        let n = puts(seen).len() as i64;
        (200, doc_json("walk", 10 + n, "Walk"))
    });
    let (link, _dir) = sol.paired("terra", "u1").await;
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
    let sent = sol.puts();
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
    let sol = sol_with(|body, seen| {
        let n = puts(seen).len();
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
    });
    let (link, _dir) = sol.paired("terra", "u1").await;
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
        link.unsent().await.unwrap() == 0 && sol.puts().len() == 2
    })
    .await;
    assert_eq!(sol.puts()[1]["if_version"], 9);
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
    let sol = sol_with(|_, _| {
        (
            409,
            format!(
                r#"{{"error":"conflict","message":"changed","current":{}}}"#,
                doc_json("walk", 8, "Mine")
            ),
        )
    });
    let (link, _dir) = sol.paired("terra", "u1").await;
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
    let sol = sol_with(|_, seen| {
        if puts(seen).len() == 1 {
            (
                413,
                r#"{"error":"too_large","message":"a document is at most 1 MiB"}"#.into(),
            )
        } else {
            (200, doc_json("read", 3, "Read"))
        }
    });
    let (link, _dir) = sol.paired("terra", "u1").await;
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
async fn a_damaged_write_is_kept_and_the_rest_go() {
    let sol = sol_with(|_, _| (200, doc_json("read", 3, "Read")));
    let (link, _dir) = sol.paired("terra", "u1").await;
    link.0
        .db
        .call(|c| {
            c.execute(
                "INSERT INTO pending (kind, collection, id, body) VALUES ('put', 'habits', 'walk', '{not json')",
                [],
            )
        })
        .await
        .unwrap()
        .unwrap();
    link.put("habits", "read", json!({ "name": "Read" }))
        .await
        .unwrap();
    link.start(Arc::new(Ignore));
    until("both to settle", async || link.unsent().await.unwrap() == 0).await;
    let kept = link.kept_writes().await.unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].why, Why::Rejected);
    assert!(kept[0].message.as_deref().unwrap().contains("damaged"));
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
    let (link, dir) = offline("terra").await;
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
    let link = crate::testing::open("terra", dir.path()).await;
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
