//! `media_control` against a real session bus, with a fake player under its
//! own name. Commands go **only to the fake**: whatever the user has
//! playing is listed (read-only) but never sent anything.
//!
//! Skipped, saying so, where there is no session bus (a bare CI runner).

use std::sync::{Arc, Mutex};

use cosmo_tools::media::{Status, players, send};

struct FakePlayer {
    status: Arc<Mutex<String>>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    fn pause(&self) {
        self.calls.lock().unwrap().push("Pause");
        *self.status.lock().unwrap() = "Paused".into();
    }
    fn play(&self) {
        self.calls.lock().unwrap().push("Play");
        *self.status.lock().unwrap() = "Playing".into();
    }
    fn next(&self) {
        self.calls.lock().unwrap().push("Next");
    }
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn a_fake_player_is_found_and_controlled() {
    let Ok(conn) = zbus::Connection::session().await else {
        eprintln!("no session bus: skipped");
        return;
    };
    let name = format!("org.mpris.MediaPlayer2.cosmotest{}", std::process::id());
    let status = Arc::new(Mutex::new("Playing".to_owned()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fake = FakePlayer {
        status: Arc::clone(&status),
        calls: Arc::clone(&calls),
    };
    let server = zbus::connection::Builder::session()
        .unwrap()
        .name(name.as_str())
        .unwrap()
        .serve_at("/org/mpris/MediaPlayer2", fake)
        .unwrap()
        .build()
        .await
        .unwrap();

    let found = players(&conn).await.unwrap();
    println!("players on this bus: {found:?}");
    assert!(
        found.contains(&(name.clone(), Status::Playing)),
        "{found:?}"
    );

    send(&conn, &name, "pause").await.unwrap();
    send(&conn, &name, "next").await.unwrap();
    send(&conn, &name, "play").await.unwrap();
    assert_eq!(*calls.lock().unwrap(), ["Pause", "Next", "Play"]);
    assert!(
        players(&conn)
            .await
            .unwrap()
            .contains(&(name.clone(), Status::Playing))
    );
    drop(server);
}
