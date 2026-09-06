//! A dumb JSON relay, grouped by pairing code. It has no idea what WebRTC
//! is — it just delivers whatever a connected client sends to whichever
//! other client(s) in the same code group the message is addressed to (or
//! to everyone else, for `PeerJoined`/broadcast signals). This mirrors
//! backend/src/sockets/quickPair.ts's message shape closely enough that the
//! shared frontend code can speak the same protocol to either relay.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

use crate::oauth::{oauth_callback_handler, OAuthState};

type PeerId = String;
type Code = String;
type RoomMember = (String /* name */, mpsc::UnboundedSender<String>);

#[derive(Clone, Default)]
struct RelayState {
    rooms: Arc<Mutex<HashMap<Code, HashMap<PeerId, RoomMember>>>>,
}

// NOTE: `rename_all` on the enum only renames the variant tag itself (e.g.
// Join -> "join") — it does NOT cascade into each variant's own fields,
// contrary to what the previous version of this file assumed. That silently
// broke every single join/signal message from day one (Deserialize failed
// with "missing field `peer_id`" against real camelCase JSON), which is why
// hosting never actually worked regardless of network path. Explicit
// per-field #[serde(rename = "...")] removes any ambiguity about this.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ClientMessage {
    Join {
        code: String,
        #[serde(rename = "peerId")]
        peer_id: String,
        name: String,
    },
    Signal {
        code: String,
        #[serde(rename = "targetPeerId")]
        target_peer_id: Option<String>,
        kind: String,
        data: serde_json::Value,
    },
}

#[derive(Serialize, Clone)]
struct PeerInfo {
    #[serde(rename = "peerId")]
    peer_id: String,
    name: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ServerMessage {
    Joined { peers: Vec<PeerInfo> },
    PeerJoined {
        #[serde(rename = "peerId")]
        peer_id: String,
        name: String,
    },
    PeerLeft {
        #[serde(rename = "peerId")]
        peer_id: String,
    },
    Signal {
        #[serde(rename = "fromPeerId")]
        from_peer_id: String,
        kind: String,
        data: serde_json::Value,
    },
}

pub async fn serve(port: u16, oauth_state: OAuthState) {
    let relay_state = RelayState::default();
    let pairing_router = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/pair/:code", get(pair_handler))
        .with_state(relay_state);

    let oauth_router = Router::new()
        .route("/oauth/callback", get(oauth_callback_handler))
        .with_state(oauth_state);

    let router = pairing_router.merge(oauth_router);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            let _ = axum::serve(listener, router).await;
        }
        Err(err) => {
            eprintln!("SyncBlaze: couldn't start the local relay on port {port}: {err}");
        }
    }
}

async fn pair_handler(ws: WebSocketUpgrade, Path(_code): Path<String>, State(state): State<RelayState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: RelayState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let mut send_task = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if ws_sender.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    let mut joined: Option<(Code, PeerId)> = None;

    while let Some(Ok(msg)) = ws_receiver.next().await {
        let Message::Text(text) = msg else { continue };
        let parsed = match serde_json::from_str::<ClientMessage>(&text) {
            Ok(p) => p,
            Err(e) => {
                // Worth keeping visible (stderr only, no UI impact) — a
                // silent `continue` here is exactly what hid this same bug
                // undetected until it was tracked down by hand.
                eprintln!("SyncBlaze relay: failed to parse {text:?}: {e}");
                continue;
            }
        };

        match parsed {
            ClientMessage::Join { code, peer_id, name } => {
                let existing = {
                    let mut rooms = state.rooms.lock().unwrap();
                    let room = rooms.entry(code.clone()).or_default();
                    let existing: Vec<PeerInfo> = room
                        .iter()
                        .map(|(id, (n, _))| PeerInfo { peer_id: id.clone(), name: n.clone() })
                        .collect();
                    room.insert(peer_id.clone(), (name.clone(), tx.clone()));
                    existing
                };

                let _ = tx.send(json(&ServerMessage::Joined { peers: existing }));
                broadcast_except(&state, &code, &peer_id, &json(&ServerMessage::PeerJoined { peer_id: peer_id.clone(), name }));

                joined = Some((code, peer_id));
            }
            ClientMessage::Signal { code, target_peer_id, kind, data } => {
                let Some((_, from_peer_id)) = &joined else { continue };
                let payload = json(&ServerMessage::Signal { from_peer_id: from_peer_id.clone(), kind, data });

                let rooms = state.rooms.lock().unwrap();
                if let Some(room) = rooms.get(&code) {
                    match target_peer_id {
                        Some(target) => {
                            if let Some((_, sender)) = room.get(&target) {
                                let _ = sender.send(payload);
                            }
                        }
                        None => {
                            for (id, (_, sender)) in room.iter() {
                                if id != from_peer_id {
                                    let _ = sender.send(payload.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some((code, peer_id)) = joined {
        let mut rooms = state.rooms.lock().unwrap();
        if let Some(room) = rooms.get_mut(&code) {
            room.remove(&peer_id);
            let left = json(&ServerMessage::PeerLeft { peer_id });
            for (_, sender) in room.values() {
                let _ = sender.send(left.clone());
            }
            if room.is_empty() {
                rooms.remove(&code);
            }
        }
    }

    send_task.abort();
}

fn broadcast_except(state: &RelayState, code: &str, except_peer_id: &str, payload: &str) {
    let rooms = state.rooms.lock().unwrap();
    if let Some(room) = rooms.get(code) {
        for (id, (_, sender)) in room.iter() {
            if id != except_peer_id {
                let _ = sender.send(payload.to_string());
            }
        }
    }
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
