//! The SDK against a real in-process server.

use std::net::SocketAddr;
use std::sync::Arc;

use cogling_eventlog::NullLogStore;
use cogling_game_core::{GameRegistry, Seed};
use cogling_protocol::{ClientMessage, CoglingInfo, ErrorCode};
use cogling_sdk::{ClientConfig, ClientError, Matchmaking, create_game, run, strategies};
use cogling_server::{Server, ServerConfig};

async fn start() -> SocketAddr {
    let mut registry = GameRegistry::new();
    registry
        .register(cogling_tictactoe::TicTacToe)
        .expect("register tictactoe");
    registry
        .register(cogling_connect4::ConnectFour)
        .expect("register connect4");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let server = Server::new(registry, ServerConfig::default(), Arc::new(NullLogStore));
    tokio::spawn(server.serve(listener, std::future::pending()));
    addr
}

fn config(addr: SocketAddr, name: &str, matchmaking: Matchmaking, games: u32) -> ClientConfig {
    ClientConfig {
        games,
        ..ClientConfig::new(
            format!("ws://{addr}/ws"),
            CoglingInfo::new(name, Some("1.0")).expect("valid name"),
            matchmaking,
        )
    }
}

#[tokio::test]
async fn rules_beats_random_over_several_games() {
    let addr = start().await;
    let rules_cfg = config(addr, "rules", Matchmaking::queue("connect4"), 6);
    let random_cfg = config(addr, "random", Matchmaking::queue("connect4"), 6);
    let mut rules = strategies::for_game("connect4", "rules", Seed::new(1)).unwrap();
    let mut random = strategies::for_game("connect4", "random", Seed::new(2)).unwrap();
    let (a, b) = tokio::join!(run(&rules_cfg, &mut rules), run(&random_cfg, &mut random));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.len(), 6);
    assert_eq!(b.len(), 6);
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.game_id, y.game_id);
        assert_eq!(x.result, y.result);
        assert_ne!(x.seat, y.seat);
    }
    let wins = a.iter().filter(|s| s.won()).count();
    // Seating is random per game; rules wins ~95% vs random, so 4+ of 6 is robust.
    assert!(wins >= 4, "rules won only {wins}/6");
}

#[tokio::test]
async fn lobby_via_create_and_join() {
    let addr = start().await;
    let creator = config(addr, "creator", Matchmaking::queue("tictactoe"), 1);
    let game_id = create_game(
        &creator,
        ClientMessage::CreateGame {
            game: "tictactoe".into(),
            seats: None,
            config: None,
            think_time_ms: Some(500),
            observation_mode: None,
        },
    )
    .await
    .unwrap();
    let mut x = strategies::for_game("tictactoe", "rules", Seed::new(3)).unwrap();
    let mut o = strategies::for_game("tictactoe", "rules", Seed::new(4)).unwrap();
    let x_cfg = config(addr, "x", Matchmaking::Join(game_id), 1);
    let o_cfg = config(addr, "o", Matchmaking::Join(game_id), 1);
    let (a, b) = tokio::join!(run(&x_cfg, &mut x), run(&o_cfg, &mut o));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a[0].game_id, game_id);
    assert_eq!(a[0].result, b[0].result);
}

#[tokio::test]
async fn blocking_errors_surface() {
    let addr = start().await;
    let mut cogling = strategies::for_game("chess", "random", Seed::new(0)).unwrap();
    let err = run(
        &config(addr, "lost", Matchmaking::queue("chess"), 1),
        &mut cogling,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        ClientError::Server {
            code: ErrorCode::UnknownGame,
            ..
        }
    ));
    // An unreachable server fails fast.
    let unreachable = config(
        "127.0.0.1:1".parse().expect("addr"),
        "x",
        Matchmaking::queue("tictactoe"),
        1,
    );
    assert!(matches!(
        run(&unreachable, &mut cogling).await,
        Err(ClientError::WebSocket(_))
    ));
}
