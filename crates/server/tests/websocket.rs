//! End-to-end tests over real WebSocket connections, using only the public API.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use cogling_connect4::ConnectFour;
use cogling_eventlog::{GameLog, MemoryLogStore, Replayer};
use cogling_game_core::{GameRegistry, GameResult};
use cogling_protocol::{ClientMessage, CoglingInfo, ErrorCode, PROTOCOL_VERSION, ServerMessage};
use cogling_server::{Server, ServerConfig};
use cogling_tictactoe::TicTacToe;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn registry() -> GameRegistry {
    let mut r = GameRegistry::new();
    r.register(TicTacToe).expect("register tictactoe");
    r.register(ConnectFour).expect("register connect4");
    r
}

async fn start(config: ServerConfig) -> (SocketAddr, MemoryLogStore) {
    let logs = MemoryLogStore::new();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let server = Server::new(registry(), config, Arc::new(logs.clone()));
    tokio::spawn(server.serve(listener, std::future::pending()));
    (addr, logs)
}

struct Client {
    ws: Ws,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("websocket handshake");
        Self { ws }
    }

    async fn hello(addr: SocketAddr, name: &str) -> Self {
        let mut c = Self::connect(addr).await;
        c.send(&ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            cogling: CoglingInfo::new(name, Some("0.1")).expect("valid cogling name"),
            api_key: None,
        })
        .await;
        assert!(matches!(
            c.recv().await,
            Some(ServerMessage::Welcome { .. })
        ));
        c
    }

    async fn send(&mut self, msg: &ClientMessage) {
        self.send_raw(&msg.to_json().expect("serializable message"))
            .await;
    }

    async fn send_raw(&mut self, text: &str) {
        self.ws.send(Message::text(text)).await.expect("send frame");
    }

    /// Next server message, or `None` once the server closed the socket.
    async fn recv(&mut self) -> Option<ServerMessage> {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
                .await
                .expect("timed out waiting for the server");
            match frame {
                Some(Ok(Message::Text(text))) => {
                    return Some(
                        ServerMessage::from_json(text.as_str()).expect("valid server message"),
                    );
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
                Some(Ok(_)) => {}
            }
        }
    }

    async fn expect_error(&mut self) -> (ErrorCode, bool) {
        match self.recv().await {
            Some(ServerMessage::Error { code, fatal, .. }) => (code, fatal),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    /// Plays the first legal action every turn until the game ends.
    async fn play_first_legal(&mut self) -> GameResult {
        loop {
            match self.recv().await.expect("socket closed mid-game") {
                ServerMessage::Decide {
                    game_id,
                    decision_id,
                    legal_actions,
                    ..
                } => {
                    let action = legal_actions.expect("state_and_legal mode")[0].clone();
                    self.send(&ClientMessage::Action {
                        game_id,
                        decision_id,
                        action,
                    })
                    .await;
                }
                ServerMessage::GameOver { result, .. } => return result,
                _ => {}
            }
        }
    }
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .expect("read response");
    response
}

#[tokio::test]
async fn coglings_play_connect4_over_websockets() {
    let (addr, logs) = start(ServerConfig::default()).await;
    let mut a = Client::hello(addr, "alpha").await;
    let mut b = Client::hello(addr, "beta").await;
    for c in [&mut a, &mut b] {
        c.send(&ClientMessage::Queue {
            game: "connect4".into(),
            seats: None,
            config: None,
        })
        .await;
    }
    let (ra, rb) = tokio::join!(a.play_first_legal(), b.play_first_legal());
    assert_eq!(ra, rb);
    // Both always play column 0 first, so the first seat stacks four.
    assert!(matches!(ra, GameResult::Completed { .. }), "{ra:?}");

    // The connection returns to idle and can queue again.
    a.send(&ClientMessage::Queue {
        game: "tictactoe".into(),
        seats: None,
        config: None,
    })
    .await;
    assert!(matches!(a.recv().await, Some(ServerMessage::Queued { .. })));

    tokio::time::sleep(Duration::from_millis(50)).await;
    let logs = logs.logs();
    assert_eq!(logs.len(), 1);
    let log = GameLog::read(std::io::Cursor::new(&logs[0])).unwrap();
    Replayer::new(&registry(), &log)
        .unwrap()
        .run_to_end()
        .unwrap();

    let status = http_get(addr, "/").await;
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    let body: Value = serde_json::from_str(status.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["protocol"], 1);
    assert_eq!(body["queued"], 1);
    assert_eq!(body["games"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn lobby_flow_over_websockets() {
    let (addr, _) = start(ServerConfig::default()).await;
    let mut a = Client::hello(addr, "alpha").await;
    let mut b = Client::hello(addr, "beta").await;
    a.send(&ClientMessage::CreateGame {
        game: "tictactoe".into(),
        seats: None,
        config: Some(serde_json::json!({"first_seat": 1})),
        think_time_ms: Some(2_000),
        observation_mode: Some(cogling_protocol::ObservationMode::State),
    })
    .await;
    let Some(ServerMessage::GameCreated { game_id }) = a.recv().await else {
        panic!("expected game_created");
    };
    a.send(&ClientMessage::Join { game_id }).await;
    assert!(matches!(
        a.recv().await,
        Some(ServerMessage::Joined {
            seats_filled: 1,
            ..
        })
    ));
    b.send(&ClientMessage::Join { game_id }).await;
    assert!(matches!(
        b.recv().await,
        Some(ServerMessage::Joined {
            seats_filled: 2,
            ..
        })
    ));
    let Some(ServerMessage::GameStart { think_time_ms, .. }) = b.recv().await else {
        panic!("expected game_start");
    };
    assert_eq!(think_time_ms, 2_000);
    // first_seat = 1: the second joiner moves first, and gets no legal list.
    let Some(ServerMessage::Decide {
        legal_actions,
        observation,
        ..
    }) = b.recv().await
    else {
        panic!("expected decide");
    };
    assert!(legal_actions.is_none());
    assert_eq!(observation["mark"], "O");
}

#[tokio::test]
async fn protocol_violations() {
    let (addr, _) = start(ServerConfig {
        max_violations: 2,
        ..ServerConfig::default()
    })
    .await;

    // The first message must be hello.
    let mut c = Client::connect(addr).await;
    c.send(&ClientMessage::Ping { nonce: None }).await;
    assert_eq!(c.expect_error().await, (ErrorCode::HelloRequired, true));
    assert!(c.recv().await.is_none());

    // Protocol version mismatch is fatal.
    let mut c = Client::connect(addr).await;
    c.send_raw(r#"{"type":"hello","protocol":99,"cogling":{"name":"x"}}"#)
        .await;
    assert_eq!(c.expect_error().await, (ErrorCode::ProtocolVersion, true));
    assert!(c.recv().await.is_none());

    // Malformed frames are reported; too many close the connection.
    let mut c = Client::hello(addr, "sloppy").await;
    c.send_raw("{not json").await;
    assert_eq!(c.expect_error().await, (ErrorCode::Malformed, false));
    c.send(&ClientMessage::Ping { nonce: Some(7) }).await;
    assert!(matches!(
        c.recv().await,
        Some(ServerMessage::Pong { nonce: Some(7) })
    ));
    c.send(&ClientMessage::Action {
        game_id: cogling_protocol::GameId::new(),
        decision_id: cogling_protocol::DecisionId::FIRST,
        action: Value::Null,
    })
    .await;
    assert_eq!(
        c.expect_error().await,
        (ErrorCode::UnexpectedMessage, false)
    );
    c.ws.send(Message::binary(vec![1, 2, 3])).await.unwrap();
    assert_eq!(c.expect_error().await, (ErrorCode::Malformed, false));
    c.send_raw(r#"{"type":"teleport"}"#).await;
    assert_eq!(c.expect_error().await, (ErrorCode::Malformed, true));
    assert!(c.recv().await.is_none());

    // Unknown games and missing sessions are reported, not fatal.
    let mut c = Client::hello(addr, "lost").await;
    c.send(&ClientMessage::Queue {
        game: "chess".into(),
        seats: None,
        config: None,
    })
    .await;
    assert_eq!(c.expect_error().await, (ErrorCode::UnknownGame, false));
    c.send(&ClientMessage::Join {
        game_id: cogling_protocol::GameId::new(),
    })
    .await;
    assert_eq!(c.expect_error().await, (ErrorCode::NoSuchGame, false));
}

#[tokio::test]
async fn limits_are_enforced() {
    let (addr, _) = start(ServerConfig {
        max_message_bytes: 1_024,
        max_messages_per_second: 2,
        max_violations: 100,
        ..ServerConfig::default()
    })
    .await;

    let mut c = Client::hello(addr, "chatty").await;
    for _ in 0..10 {
        c.send(&ClientMessage::Ping { nonce: None }).await;
    }
    let mut limited = 0;
    for _ in 0..10 {
        if let Some(ServerMessage::Error { code, .. }) = c.recv().await {
            assert_eq!(code, ErrorCode::RateLimited);
            limited += 1;
        }
    }
    // The bucket allows a burst of 2x the rate (hello consumed one token).
    assert!(limited >= 6, "only {limited} messages were rate limited");

    let mut big = Client::hello(addr, "big").await;
    let padding = "x".repeat(4_096);
    big.send_raw(&format!(r#"{{"type":"ping","pad":"{padding}"}}"#))
        .await;
    assert!(
        big.recv().await.is_none(),
        "oversized frames close the socket"
    );
}

#[tokio::test]
async fn connection_limit() {
    let (addr, _) = start(ServerConfig {
        max_connections: 1,
        ..ServerConfig::default()
    })
    .await;
    let _first = Client::hello(addr, "first").await;
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await;
    assert!(err.is_err(), "second connection should be refused");
}
