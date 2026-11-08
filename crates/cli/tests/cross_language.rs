//! Definition-of-success check: a Rust cogling and a Python cogling play Tic-Tac-Toe
//! and Connect Four against each other through a local server, and every game
//! log replays exactly.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use cogling_cli::{ReplayArgs, RunArgs, builtin_registry};
use cogling_eventlog::FileLogStore;
use cogling_protocol::GameId;
use cogling_server::{Server, ServerConfig};

fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| {
        Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

fn python_cogling(script: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../clients/python")
        .join(script)
}

async fn play(url: &str, game: &str, py: &'static str, script: &str, games: u32) {
    let script = python_cogling(script);
    let py_url = url.to_owned();
    let py_game = game.to_owned();
    let python = tokio::task::spawn_blocking(move || {
        Command::new(py)
            .arg(&script)
            .args([
                "--url",
                &py_url,
                "--game",
                &py_game,
                "--games",
                &games.to_string(),
            ])
            .args(["--seed", "1"])
            .output()
            .expect("run python cogling")
    });
    let rust = cogling_cli::run(RunArgs {
        url: url.to_owned(),
        game: game.to_owned(),
        strategy: "rules".into(),
        name: Some("rust-rules".into()),
        version: "1.0".into(),
        games,
        join: None,
        seed: Some(2),
    });
    let (python, rust) = tokio::join!(python, rust);
    let python = python.expect("python task");
    let rust = rust.expect("rust cogling");
    assert!(
        python.status.success(),
        "python cogling failed: {}",
        String::from_utf8_lossy(&python.stderr)
    );
    assert_eq!(rust.len(), games as usize);
    let stdout = String::from_utf8_lossy(&python.stdout);
    for summary in &rust {
        assert!(
            stdout.contains(&summary.game_id.to_string()),
            "python did not play {}: {stdout}",
            summary.game_id
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_and_python_coglings_play_both_games() {
    let Some(py) = python() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    let dir = std::env::temp_dir().join(format!("cogling-e2e-{}", GameId::new()));
    let logs = FileLogStore::new(&dir).expect("log dir");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("ws://{}/ws", listener.local_addr().expect("addr"));
    let server = Server::new(
        builtin_registry().expect("registry"),
        ServerConfig::default(),
        Arc::new(logs),
    );
    tokio::spawn(server.serve(listener, std::future::pending()));

    play(&url, "tictactoe", py, "random_cogling.py", 3).await;
    play(&url, "connect4", py, "rules_cogling.py", 3).await;

    // Logs are flushed when sessions end; allow them a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read log dir")
        .map(|e| e.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 6);
    for file in files {
        let log = cogling_cli::read_log(&file).expect("read log");
        let names: Vec<&str> = log
            .header()
            .players
            .iter()
            .map(|p| p.cogling.name.as_str())
            .collect();
        assert!(names.contains(&"rust-rules"), "{names:?}");
        let mut out = Vec::new();
        cogling_cli::replay(
            &ReplayArgs {
                file,
                view: "public".into(),
                verify: true,
            },
            &mut out,
        )
        .expect("replay verifies");
    }
    std::fs::remove_dir_all(dir).expect("cleanup");
}
