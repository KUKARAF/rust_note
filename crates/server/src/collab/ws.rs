//! WebSocket handlers for collaborative editing sessions: the authenticated
//! `/ws/notes/{*note_id}` handler and the public, share-token-authenticated
//! `/ws/shared/{token}` guest handler. Both drive the exact same
//! [`handle_socket`] sync loop against the exact same [`super::room::Room`]
//! (keyed by note id in the shared [`super::room::RoomRegistry`]) — they only
//! differ in *how* they establish the note id and read/write permission
//! before the upgrade.
//!
//! Route: `GET /ws/notes/{*note_id}` (the `{*note_id}` wildcard matches note
//! ids containing `/`, exactly like the REST `/api/notes/{*path}` route).
//!
//! # Auth (before upgrade)
//!
//! The session rides along on the upgrade request's cookie, so the same
//! [`session::effective_user_id`] used by the REST routes resolves the user
//! (returning `"admin"` in dev mode). The mobile app cannot send that
//! cookie cross-site, so it authenticates with `?token=<device token>`
//! instead (resolved via [`crate::auth::device_token`]); a present-but-
//! invalid token is a hard 401 with no session fallback, mirroring
//! `RequireAuth`. The token travels in the query string because the browser
//! WebSocket API cannot set headers — the server does no request logging
//! (there is no `TraceLayer`), but a reverse proxy's access logs may capture
//! it; operators should strip or disable query logging for `/ws/notes/*`.
//! The connection is rejected before the
//! upgrade unless the note exists and the user can read it. Write permission
//! ([`acl::can_write`]) decides whether the socket's doc-mutating messages
//! are applied — a read-only user still connects and sees live edits and
//! peers' cursors, but their `Update`/`SyncStep2` frames are ignored.
//!
//! # Guest access (`/ws/shared/{token}`)
//!
//! See [`guest_ws_handler`] below for the guest counterpart: a share token
//! (resolved via [`crate::share::store::resolve`]) stands in for the session,
//! and the link's `permission` ("view"/"edit") stands in for `can_write`. A
//! token is only checked at *connect* time — if it expires mid-session the
//! connection is allowed to run to its natural end (disconnect), but a fresh
//! connection attempt after expiry is rejected. This is a deliberate,
//! documented simplification for v1.
//!
//! # Wire protocol
//!
//! Standard Yjs `y-protocols` framing (see [`super::protocol`]), byte-for-byte
//! compatible with the JS `y-websocket` provider: binary WebSocket frames
//! carrying `MSG_SYNC` (SyncStep1/SyncStep2/Update) and `MSG_AWARENESS`
//! messages. On connect the server sends `SyncStep1` (its state vector) plus
//! the room's current awareness, then relays.
//!
//! # Assigned cursor color
//!
//! The client (`y-codemirror.next`) sets its own awareness `user` field; the
//! server only *relays* awareness. As a convenience the server also computes
//! a deterministic color for the user (same HSL-from-hash used for guests,
//! [`share_identity::color_for_id`]) and exposes it on the upgrade response's
//! `x-collab-user-color` / `x-collab-display-name` headers, so a client that
//! wants a server-authoritative color can read it. Clients are free to
//! compute their own instead.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_sessions::Session;
use yrs::encoding::read::Cursor;
use yrs::sync::{Message, MessageReader, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{ClientID, ReadTxn, Transact, Update};

use super::room::Room;
use crate::auth::{session, share_identity};
use crate::notes::acl;
use crate::notes::fs_store::{is_valid_note_id, path_to_note_id};
use crate::state::AppState;

/// Collab WebSocket router, merged into the top-level router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ws/notes/{*note_id}", get(ws_handler))
        .route("/ws/shared/{token}", get(guest_ws_handler))
}

/// Query parameters for the authenticated collab WebSocket.
#[derive(serde::Deserialize)]
struct CollabWsQuery {
    /// Device token (the mobile app's credential — see module docs).
    token: Option<String>,
}

/// Authenticate + authorize, then upgrade the connection into the sync loop.
async fn ws_handler(
    State(state): State<AppState>,
    Path(raw_path): Path<String>,
    axum::extract::Query(query): axum::extract::Query<CollabWsQuery>,
    session: Session,
    ws: WebSocketUpgrade,
) -> Response {
    // Resolve the user: dev mode -> "admin"; a `?token=` device token if
    // present (hard 401 when invalid — no session fallback, so revocation
    // is authoritative); otherwise the session cookie.
    let user_id = if state.config.dev_mode {
        crate::config::DEV_MODE_USER_ID.to_string()
    } else if let Some(token) = query.token {
        match crate::auth::device_token::resolve(&state.db, &token).await {
            Ok(Some(user_id)) => user_id,
            Ok(None) => return (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
            Err(err) => {
                tracing::error!(error = %err, "collab: device token resolve failed");
                return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
            }
        }
    } else {
        match session::effective_user_id(&state, &session).await {
            Some(user_id) => user_id,
            None => return (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
        }
    };

    // Note id handling mirrors the REST routes exactly.
    let note_id = path_to_note_id(&raw_path);
    if !is_valid_note_id(&note_id) {
        return (StatusCode::BAD_REQUEST, "invalid note id").into_response();
    }

    match acl::note_exists(&state.db, &note_id).await {
        Ok(true) => {}
        Ok(false) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            tracing::error!(error = %err, "collab: note_exists check failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    }

    let can_read = acl::can_read(&state.db, &note_id, &user_id)
        .await
        .unwrap_or(false);
    if !can_read {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let can_write = acl::can_write(&state.db, &note_id, &user_id)
        .await
        .unwrap_or(false);

    // Attach to (or lazily create) the room. Balanced by `ConnGuard` below.
    let room = state.rooms.get_or_create(&note_id, &user_id, &state).await;

    // Convenience metadata for the client (optional — see module docs).
    let color = share_identity::color_for_id(&user_id);
    let display_name = display_name_for(&state, &user_id).await;

    let mut response = ws.on_upgrade(move |socket| handle_socket(socket, state, room, can_write));
    if let Ok(v) = HeaderValue::from_str(&color) {
        response.headers_mut().insert("x-collab-user-color", v);
    }
    if let Ok(v) = HeaderValue::from_str(&display_name) {
        response.headers_mut().insert("x-collab-display-name", v);
    }
    response
}

/// Look up a user's display name, falling back to the id.
async fn display_name_for(state: &AppState, user_id: &str) -> String {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT display_name FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    row.and_then(|(name,)| name)
        .unwrap_or_else(|| user_id.to_string())
}

/// Public guest collab WebSocket: `GET /ws/shared/{token}`. No session, no
/// login — the share token itself is the credential. Resolves the token to a
/// `(note_id, permission)` pair exactly like the guest REST resolve endpoint
/// (`GET /api/shared/{token}`, see `crate::share`), then joins the *same*
/// room the authenticated `/ws/notes/{note_id}` handler would for that note,
/// so a guest and the owner collaborate live in one document.
///
/// An invalid or expired token is rejected with 404 before the upgrade — see
/// `share::store::resolve`'s docs for why both cases collapse to the same
/// response (no enumeration signal). If the note itself has since been
/// deleted the connection is also rejected with 404.
async fn guest_ws_handler(
    State(state): State<AppState>,
    Path(token): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let link = match crate::share::store::resolve(&state.db, &token).await {
        Ok(Some(link)) => link,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "invalid or expired share link").into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, "collab: share token resolve failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };

    match acl::note_exists(&state.db, &link.note_id).await {
        Ok(true) => {}
        Ok(false) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            tracing::error!(error = %err, "collab: note_exists check failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    }

    let can_write = matches!(link.permission, crate::share::store::SharePermission::Edit);

    // Attribute a freshly-created room to the link's real owner (not the
    // guest), matching `owner_hint`'s role for the authenticated path — see
    // `room::build_room` / `persist::flush_room`'s new-note safety net.
    let room = state
        .rooms
        .get_or_create(&link.note_id, &link.owner_id, &state)
        .await;

    // Fresh guest identity per connection (v1: no reconnect-stable identity
    // cookie yet — see `auth::share_identity` module docs for the cookie
    // primitive this could later ride on).
    let guest = share_identity::GuestIdentity::new_random();

    let mut response = ws.on_upgrade(move |socket| handle_socket(socket, state, room, can_write));
    if let Ok(v) = HeaderValue::from_str(&guest.color) {
        response.headers_mut().insert("x-collab-user-color", v);
    }
    if let Ok(v) = HeaderValue::from_str(&guest.display_name) {
        response.headers_mut().insert("x-collab-display-name", v);
    }
    response
}

/// Ensures the room connection count is decremented (and the reaper
/// scheduled) no matter how the socket task ends.
struct ConnGuard {
    room: Arc<Room>,
    state: AppState,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.state
            .rooms
            .release(self.room.clone(), self.state.clone());
    }
}

/// Drive one WebSocket connection's sync loop.
async fn handle_socket(mut socket: WebSocket, state: AppState, room: Arc<Room>, can_write: bool) {
    let _guard = ConnGuard {
        room: room.clone(),
        state,
    };
    let conn_id = room.next_conn_id();
    let mut rx = room.subscribe();

    // Awareness client ids this connection has introduced, so we can retract
    // their cursors when it disconnects.
    let mut controlled: HashSet<ClientID> = HashSet::new();

    // Initial handshake: SyncStep1 (our state vector) + current awareness of
    // everyone already in the room, so a fresh client immediately learns the
    // doc state and existing peers' cursors.
    for frame in initial_frames(&room) {
        if socket
            .send(WsMessage::Binary(Bytes::from(frame)))
            .await
            .is_err()
        {
            return;
        }
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(WsMessage::Binary(data))) => {
                        // Handle the frame (mutations + relays happen inside),
                        // then send any replies destined for this socket.
                        let replies = process_frame(
                            &room, &data, conn_id, can_write, &mut controlled,
                        );
                        if send_replies(&mut socket, replies).await.is_break() {
                            break;
                        }
                    }
                    // y-protocol frames are always binary; ignore text.
                    Some(Ok(WsMessage::Text(_))) => {}
                    Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => {}
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(_)) => break,
                }
            }
            relayed = rx.recv() => {
                match relayed {
                    Ok(msg) => {
                        // Skip our own echo.
                        if msg.origin != conn_id
                            && socket.send(WsMessage::Binary(msg.data)).await.is_err()
                        {
                            break;
                        }
                    }
                    // Fell behind the fan-out; the client's periodic sync will
                    // reconcile any missed frames.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    retract_awareness(&room, conn_id, &controlled);
    // `_guard` drops here -> release + maybe reap.
}

/// Encode the connect-time frames (`SyncStep1` + full awareness).
fn initial_frames(room: &Arc<Room>) -> Vec<Vec<u8>> {
    let awareness = room.lock_awareness();
    let sv = awareness.doc().transact().state_vector();
    let mut frames = vec![Message::Sync(SyncMessage::SyncStep1(sv)).encode_v1()];
    if let Ok(update) = awareness.update() {
        if !update.clients.is_empty() {
            frames.push(Message::Awareness(update).encode_v1());
        }
    }
    frames
}

/// Retract the cursors this connection owned when it disconnects, so peers
/// drop them immediately instead of waiting for their awareness timeout.
fn retract_awareness(room: &Arc<Room>, conn_id: u64, controlled: &HashSet<ClientID>) {
    if controlled.is_empty() {
        return;
    }
    let frame = {
        let awareness = room.lock_awareness();
        for &client_id in controlled {
            awareness.remove_state(client_id);
        }
        awareness
            .update_with_clients(controlled.iter().copied())
            .ok()
            .map(|u| Message::Awareness(u).encode_v1())
    };
    if let Some(frame) = frame {
        // `conn_id` is unique to this (leaving) connection, so every *other*
        // socket receives the retraction.
        room.broadcast_frame(conn_id, Bytes::from(frame));
    }
}

/// Send this socket's pending replies; `Break` if the socket is gone.
async fn send_replies(socket: &mut WebSocket, replies: Vec<Vec<u8>>) -> ControlFlow<()> {
    for reply in replies {
        if socket
            .send(WsMessage::Binary(Bytes::from(reply)))
            .await
            .is_err()
        {
            return ControlFlow::Break(());
        }
    }
    ControlFlow::Continue(())
}

/// Decode and act on one inbound binary frame (which may pack several
/// y-protocol messages). Doc/awareness mutations are applied to the room and
/// relayed to the rest of the room here; returns the frames that must be sent
/// back to *this* socket (e.g. a SyncStep2 in reply to a SyncStep1).
fn process_frame(
    room: &Arc<Room>,
    data: &[u8],
    conn_id: u64,
    can_write: bool,
    controlled: &mut HashSet<ClientID>,
) -> Vec<Vec<u8>> {
    // Replies to send back to *this* socket (e.g. SyncStep2 for a SyncStep1).
    let mut replies: Vec<Vec<u8>> = Vec::new();

    let mut decoder = DecoderV1::new(Cursor::new(data));
    let reader = MessageReader::new(&mut decoder);
    for result in reader {
        let message = match result {
            Ok(m) => m,
            Err(_) => break, // malformed frame; stop parsing it
        };
        match message {
            // Peer announced its state vector: reply with what it's missing.
            Message::Sync(SyncMessage::SyncStep1(sv)) => {
                let update = {
                    let awareness = room.lock_awareness();
                    let out = awareness.doc().transact().encode_state_as_update_v1(&sv);
                    out
                };
                replies.push(Message::Sync(SyncMessage::SyncStep2(update)).encode_v1());
            }
            // A doc delta (reply to our SyncStep1, or a live edit). Apply it
            // to the room doc and relay it to the other sockets.
            Message::Sync(SyncMessage::SyncStep2(update))
            | Message::Sync(SyncMessage::Update(update)) => {
                if !can_write {
                    continue; // read-only connection: ignore doc mutations
                }
                let applied = {
                    let awareness = room.lock_awareness();
                    let doc = awareness.doc();
                    match Update::decode_v1(&update) {
                        Ok(decoded) => {
                            let mut txn = doc.transact_mut();
                            txn.apply_update(decoded).is_ok()
                        }
                        Err(_) => false,
                    }
                };
                if applied {
                    room.mark_dirty();
                    let frame = Message::Sync(SyncMessage::Update(update)).encode_v1();
                    room.broadcast_frame(conn_id, Bytes::from(frame));
                }
            }
            // Presence update: apply and relay (allowed even for read-only
            // connections so their cursor is visible to others).
            Message::Awareness(update) => {
                for client_id in update.clients.keys() {
                    controlled.insert(*client_id);
                }
                {
                    let awareness = room.lock_awareness();
                    let _ = awareness.apply_update(update.clone());
                }
                let frame = Message::Awareness(update).encode_v1();
                room.broadcast_frame(conn_id, Bytes::from(frame));
            }
            // Peer asked for everyone's awareness: reply to this socket only.
            Message::AwarenessQuery => {
                let reply = {
                    let awareness = room.lock_awareness();
                    awareness.update().ok()
                };
                if let Some(update) = reply {
                    replies.push(Message::Awareness(update).encode_v1());
                }
            }
            Message::Auth(_) | Message::Custom(_, _) => {}
        }
    }

    replies
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::room::CONTENT_FIELD;
    use yrs::sync::Awareness;
    use yrs::{Doc, GetString, Text};

    /// A client's SyncStep1 against a seeded room yields a SyncStep2 that
    /// carries the room's content back to the client.
    #[tokio::test]
    async fn sync_step1_replies_with_room_content() {
        let (state, _notes_dir, _db_dir) = crate::collab::test_support::test_state().await;
        state
            .notes_repo
            .write_and_commit("doc.md", "# live\n", "admin", "a@e", "seed")
            .await
            .unwrap();
        let room = state.rooms.get_or_create("doc", "admin", &state).await;

        // A fresh client with an empty doc sends its state vector.
        let client = Doc::new();
        let _ = client.get_or_insert_text(CONTENT_FIELD);
        let step1 =
            Message::Sync(SyncMessage::SyncStep1(client.transact().state_vector())).encode_v1();

        let replies = process_frame(&room, &step1, 1, true, &mut HashSet::new());
        assert_eq!(replies.len(), 1, "expected a single SyncStep2 reply");

        // The reply is a SyncStep2 whose update brings the client in sync.
        match Message::decode_v1(&replies[0]).unwrap() {
            Message::Sync(SyncMessage::SyncStep2(update)) => {
                let mut txn = client.transact_mut();
                txn.apply_update(Update::decode_v1(&update).unwrap())
                    .unwrap();
            }
            other => panic!("expected SyncStep2, got {other:?}"),
        }
        let client_text = client.get_or_insert_text(CONTENT_FIELD);
        assert_eq!(client_text.get_string(&client.transact()), "# live\n");
    }

    /// An inbound doc update is applied to the room, marks it dirty, and is
    /// relayed (tagged with the sender's conn id) to the other sockets.
    #[tokio::test]
    async fn update_applies_marks_dirty_and_relays() {
        let (state, _notes_dir, _db_dir) = crate::collab::test_support::test_state().await;
        state
            .notes_repo
            .write_and_commit("edit.md", "start\n", "admin", "a@e", "seed")
            .await
            .unwrap();
        let room = state.rooms.get_or_create("edit", "admin", &state).await;
        let mut rx = room.subscribe();

        // Peer builds a doc update carrying its own edit, expressed relative
        // to the room's current state vector (as a real client would).
        let peer = Doc::new();
        let peer_text = peer.get_or_insert_text(CONTENT_FIELD);
        {
            let mut txn = peer.transact_mut();
            peer_text.insert(&mut txn, 0, "appended ");
        }
        let update = {
            let sv = {
                let awareness = room.awareness.lock().unwrap();
                let sv = awareness.doc().transact().state_vector();
                sv
            };
            peer.transact().encode_state_as_update_v1(&sv)
        };
        let frame = Message::Sync(SyncMessage::Update(update)).encode_v1();

        let replies = process_frame(&room, &frame, 99, true, &mut HashSet::new());
        assert!(replies.is_empty(), "an Update produces no direct reply");
        assert!(room.dirty.load(std::sync::atomic::Ordering::Acquire));
        assert!(room.snapshot_text().contains("appended "));

        let relayed = rx.try_recv().expect("update relayed to the room");
        assert_eq!(relayed.origin, 99);
        match Message::decode_v1(&relayed.data).unwrap() {
            Message::Sync(SyncMessage::Update(_)) => {}
            other => panic!("expected relayed Update, got {other:?}"),
        }
    }

    /// Awareness updates are applied, relayed, and their client ids tracked
    /// so they can be retracted on disconnect.
    #[tokio::test]
    async fn awareness_relays_and_tracks_client_ids() {
        let (state, _notes_dir, _db_dir) = crate::collab::test_support::test_state().await;
        state
            .notes_repo
            .write_and_commit("aw.md", "x\n", "admin", "a@e", "seed")
            .await
            .unwrap();
        let room = state.rooms.get_or_create("aw", "admin", &state).await;
        let mut rx = room.subscribe();

        let peer = Awareness::new(Doc::new());
        let peer_client = peer.client_id();
        peer.set_local_state(serde_json::json!({"user":{"name":"rafa","color":"#f80"}}))
            .unwrap();
        let frame = Message::Awareness(peer.update().unwrap()).encode_v1();

        let mut controlled = HashSet::new();
        let replies = process_frame(&room, &frame, 42, true, &mut controlled);
        assert!(replies.is_empty());
        assert!(controlled.contains(&peer_client), "client id tracked");

        // Relayed with our origin id.
        let relayed = rx.try_recv().expect("awareness relayed");
        assert_eq!(relayed.origin, 42);
        match Message::decode_v1(&relayed.data).unwrap() {
            Message::Awareness(_) => {}
            other => panic!("expected awareness frame, got {other:?}"),
        }

        // The room now knows this peer's presence state.
        let seen: serde_json::Value = {
            let awareness = room.awareness.lock().unwrap();
            awareness.state(peer_client).unwrap()
        };
        assert_eq!(seen["user"]["name"], "rafa");

        // Retracting on disconnect broadcasts a removal to others.
        retract_awareness(&room, 42, &controlled);
        let removal = rx.try_recv().expect("retraction broadcast");
        match Message::decode_v1(&removal.data).unwrap() {
            Message::Awareness(_) => {}
            other => panic!("expected awareness retraction, got {other:?}"),
        }
    }

    /// A read-only connection's doc mutations are ignored (not applied, not
    /// relayed, room stays clean).
    #[tokio::test]
    async fn read_only_connection_cannot_mutate_doc() {
        let (state, _notes_dir, _db_dir) = crate::collab::test_support::test_state().await;
        state
            .notes_repo
            .write_and_commit("ro.md", "start\n", "admin", "a@e", "seed")
            .await
            .unwrap();
        let room = state.rooms.get_or_create("ro", "admin", &state).await;

        let peer = Doc::new();
        let peer_text = peer.get_or_insert_text(CONTENT_FIELD);
        {
            let mut txn = peer.transact_mut();
            peer_text.insert(&mut txn, 0, "HACKED ");
        }
        let update = {
            let sv = {
                let awareness = room.awareness.lock().unwrap();
                let sv = awareness.doc().transact().state_vector();
                sv
            };
            peer.transact().encode_state_as_update_v1(&sv)
        };
        let frame = Message::Sync(SyncMessage::Update(update)).encode_v1();

        let replies = process_frame(&room, &frame, 1, false, &mut HashSet::new());
        assert!(replies.is_empty());
        assert_eq!(room.snapshot_text(), "start\n");
        assert!(!room.dirty.load(std::sync::atomic::Ordering::Acquire));
    }
}
