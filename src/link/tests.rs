use serde_json::json;

use super::{cache::cursor, pairing::normalise, *};
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
