//! The game server: hosts game sessions and connects coglings over WebSockets.
//!
//! The same server runs in standalone mode (no auth, no database) and will be
//! embedded by the hosted platform. Endpoints:
//!
//! - `GET /ws` — the cogling protocol (see `docs/protocol.md`);
//! - `GET /` — JSON status: available games, live sessions, connections.
//!
//! # Examples
//!
//! ```no_run
//! # async fn run() -> std::io::Result<()> {
//! use std::sync::Arc;
//! use cogling_eventlog::NullLogStore;
//! use cogling_game_core::GameRegistry;
//! use cogling_server::{Server, ServerConfig};
//!
//! let server = Server::new(GameRegistry::new(), ServerConfig::default(), Arc::new(NullLogStore));
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:7878").await?;
//! server.serve(listener, std::future::pending()).await
//! # }
//! ```

mod config;
mod connection;
mod hub;
mod session;

pub use config::{ServerConfig, ThinkTime, ThinkTimeError};

use std::future::Future;
use std::sync::Arc;

use axum::Router;
use axum::extract::{State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use cogling_eventlog::LogStore;
use cogling_game_core::{GameInfo, GameRegistry};
use cogling_protocol::PROTOCOL_VERSION;
use serde::Serialize;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

use crate::hub::{Hub, HubStatus};

/// A configured game server.
#[derive(Debug, Clone)]
pub struct Server {
    hub: Arc<Hub>,
}

#[derive(Serialize)]
struct StatusBody<'a> {
    server: &'static str,
    version: &'static str,
    protocol: u32,
    games: Vec<&'a GameInfo>,
    #[serde(flatten)]
    live: HubStatus,
}

async fn status(State(hub): State<Arc<Hub>>) -> Response {
    let body = StatusBody {
        server: "cogling",
        version: env!("CARGO_PKG_VERSION"),
        protocol: PROTOCOL_VERSION,
        games: hub.registry.iter().map(|g| g.info()).collect(),
        live: hub.status(),
    };
    axum::Json(body).into_response()
}

async fn websocket(State(hub): State<Arc<Hub>>, ws: WebSocketUpgrade) -> Response {
    let Some(guard) = hub.try_acquire_connection() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "too many connections").into_response();
    };
    let limit = usize::try_from(hub.config.max_message_bytes).unwrap_or(usize::MAX);
    ws.max_message_size(limit)
        .max_frame_size(limit)
        .on_upgrade(move |socket| connection::handle_socket(hub, socket, guard))
}

impl Server {
    /// Creates a server for the games in `registry`, writing logs to `logs`.
    #[must_use]
    pub fn new(registry: GameRegistry, config: ServerConfig, logs: Arc<dyn LogStore>) -> Self {
        Self {
            hub: Arc::new(Hub::new(registry, config, logs)),
        }
    }

    /// The HTTP/WebSocket routes, for embedding in a larger application.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(status))
            .route("/ws", get(websocket))
            .layer(TraceLayer::new_for_http())
            .with_state(Arc::clone(&self.hub))
    }

    /// Serves until `shutdown` completes.
    ///
    /// # Errors
    ///
    /// Propagates IO errors from the listener.
    pub async fn serve(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown)
            .await
    }
}
