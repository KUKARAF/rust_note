//! CRDT sync + awareness protocol reference for the collab layer.
//!
//! This module is the Phase-2 dependency spike made concrete. It does NOT
//! implement rooms, the WS handler, or persistence (those are follow-up
//! tasks in [`super::room`], [`super::ws`], [`super::persist`]). Its job is
//! to (a) pin down the exact `yrs` API surface the collab code will use and
//! (b) prove — with checked-in tests — that document sync and awareness
//! (presence / colored cursors) round-trip and are wire-compatible with the
//! JS `y-websocket` client.
//!
//! # The stack
//!
//! Everything comes from a single crate at one version: `yrs = "0.26"`.
//! `yrs` ships the entire Yjs sync + awareness protocol in-crate under
//! [`yrs::sync`]. There is no `y-sync` / `yrs-axum` (both lag on old yrs
//! versions with an incompatible `Doc` API).
//!
//! # Wire protocol (what the JS frontend must speak)
//!
//! Messages are the standard `y-protocols` framing, encoded with lib0
//! variable-length integers. A message is:
//!
//! ```text
//! [ varint messageType ] [ type-specific body ]
//! ```
//!
//! `messageType` (see [`yrs::sync::protocol`] constants):
//!
//! | value | meaning            | body                                            |
//! |-------|--------------------|-------------------------------------------------|
//! | `0`   | `MSG_SYNC`         | `[varint syncType][...]` — the sync sub-protocol |
//! | `1`   | `MSG_AWARENESS`    | `[varint len][awareness update bytes]`          |
//! | `2`   | `MSG_AUTH`         | permission flag + reason string                 |
//! | `3`   | `MSG_QUERY_AWARENESS` | (empty) request for peers' awareness state    |
//!
//! Sync sub-types (body of a `MSG_SYNC` message):
//!
//! | value | meaning       | body                                              |
//! |-------|---------------|---------------------------------------------------|
//! | `0`   | `SyncStep1`   | `[varint len][state vector bytes]` (what I have)  |
//! | `1`   | `SyncStep2`   | `[varint len][update bytes]` (diff you're missing) |
//! | `2`   | `Update`      | `[varint len][update bytes]` (live edit broadcast) |
//!
//! Concretely, `Message::Sync(SyncStep1(empty))` encodes to the bytes
//! `[0, 0, 1, 0]` (sync, step1, len=1, one zero byte = empty state vector) —
//! byte-for-byte what `y-websocket` sends. See [`tests::sync_step1_wire_framing`].
//!
//! ## Handshake (identical to `y-websocket`)
//!
//! On connect, each side sends `SyncStep1` (its state vector) and its local
//! `Awareness` state. On receiving a peer's `SyncStep1`, reply with
//! `SyncStep2` carrying the delta the peer lacks. Subsequent local edits are
//! broadcast as `Update` messages; cursor/presence changes as `MSG_AWARENESS`
//! updates. [`yrs::sync::DefaultProtocol`] implements exactly this state
//! machine and can drive the server side directly.
//!
//! # API surface for the room/ws/persist code
//!
//! Document side (holds note markdown as one shared text named `"content"`):
//! * make a doc:            [`yrs::Doc::new`]
//! * shared text handle:    [`yrs::Doc::get_or_insert_text`] (`"content"`)
//! * edit:                  `TextRef::insert` / `remove_range` inside a
//!   `doc.transact_mut()` transaction
//! * read text:             `TextRef::get_string(&doc.transact())`
//! * SyncStep1 payload:     `doc.transact().state_vector()`
//! * SyncStep2 / diff:      `txn.encode_state_as_update_v1(&their_state_vector)`
//! * whole-doc snapshot:    `txn.encode_state_as_update_v1(&StateVector::default())`
//!   (this is how persistence serializes the doc to disk / git)
//! * apply an update:       `txn.apply_update(Update::decode_v1(bytes)?)?`
//!
//! Awareness side (presence — drives the colored per-user cursors):
//! * wrap a doc:            [`yrs::sync::Awareness::new`]
//! * this client's id:      `Awareness::client_id`
//! * publish local cursor:  `Awareness::set_local_state(serde_json_value)`
//!   — the JS convention is `{ "user": { "name": ..., "color": ... }, ... }`;
//!   `y-codemirror.next` also stuffs cursor position under this state.
//! * encode for the wire:   `Awareness::update() -> AwarenessUpdate`, then
//!   `AwarenessUpdate::encode_v1()`
//! * apply a peer's update: `AwarenessUpdate::decode_v1(bytes)?` then
//!   `Awareness::apply_update(update)?`
//! * read a peer's state:   `Awareness::state::<serde_json::Value>(client_id)`
//!
//! `Message`/`SyncMessage` (from [`yrs::sync`]) encode/decode the framing via
//! the [`Encode`](yrs::updates::encoder::Encode) /
//! [`Decode`](yrs::updates::decoder::Decode) traits (`encode_v1` /
//! `decode_v1`), so the ws handler can parse an inbound frame with
//! `Message::decode_v1(bytes)` and emit one with `msg.encode_v1()`.

#[cfg(test)]
mod tests {
    use yrs::sync::awareness::AwarenessUpdate;
    use yrs::sync::{Awareness, Message, SyncMessage};
    use yrs::updates::decoder::Decode;
    use yrs::updates::encoder::Encode;
    use yrs::{Doc, GetString, ReadTxn, Text, Transact, Update};

    const FIELD: &str = "content";

    /// Proves the document sync path: edits on one `Doc` transfer to a second
    /// `Doc` via an encoded update and the shared text converges.
    #[test]
    fn sync_update_roundtrip() {
        // Author edits their doc.
        let d1 = Doc::new();
        let text1 = d1.get_or_insert_text(FIELD);
        {
            let mut txn = d1.transact_mut();
            text1.insert(&mut txn, 0, "# Hello CRDT\n");
            text1.insert(&mut txn, 13, "second line");
        }

        // Fresh peer announces what it has (SyncStep1 payload).
        let d2 = Doc::new();
        let _ = d2.get_or_insert_text(FIELD);
        let peer_sv = d2.transact().state_vector();

        // Author computes the diff the peer is missing (SyncStep2 payload)...
        let diff = d1.transact().encode_state_as_update_v1(&peer_sv);

        // ...and the peer applies it.
        {
            let mut txn = d2.transact_mut();
            txn.apply_update(Update::decode_v1(&diff).unwrap()).unwrap();
        }

        let text2 = d2.get_or_insert_text(FIELD);
        assert_eq!(
            text2.get_string(&d2.transact()),
            "# Hello CRDT\nsecond line",
        );
        assert_eq!(
            text1.get_string(&d1.transact()),
            text2.get_string(&d2.transact()),
            "docs must converge",
        );
    }

    /// Proves the awareness/presence path that drives colored cursors: a
    /// user's `{ user: { name, color } }` state encodes, crosses the wire,
    /// and decodes intact on another peer keyed by client id.
    #[test]
    fn awareness_presence_roundtrip() {
        let a1 = Awareness::new(Doc::new());
        let client_id = a1.client_id();
        let state = serde_json::json!({
            "user": { "name": "rafa", "color": "#ff8800" }
        });
        a1.set_local_state(state.clone()).unwrap();

        // Encode -> bytes -> decode -> apply on a second peer.
        let update: AwarenessUpdate = a1.update().unwrap();
        let bytes = update.encode_v1();

        let a2 = Awareness::new(Doc::new());
        a2.apply_update(AwarenessUpdate::decode_v1(&bytes).unwrap())
            .unwrap();

        let seen: serde_json::Value = a2
            .state::<serde_json::Value>(client_id)
            .expect("peer state present after applying awareness update");
        assert_eq!(seen, state, "presence state must round-trip verbatim");
    }

    /// Locks the on-wire framing to the `y-protocols` bytes the JS
    /// `y-websocket` client expects, so the frontend agent can rely on it.
    #[test]
    fn sync_step1_wire_framing() {
        let sv = Doc::new().transact().state_vector();
        let msg = Message::Sync(SyncMessage::SyncStep1(sv));
        let bytes = msg.encode_v1();

        // messageType=MSG_SYNC(0), syncType=SyncStep1(0), len=1, empty-sv byte 0.
        assert_eq!(bytes, vec![0u8, 0, 1, 0]);
        assert_eq!(bytes[0], yrs::sync::protocol::MSG_SYNC);

        // And it decodes back to the same logical message.
        match Message::decode_v1(&bytes).unwrap() {
            Message::Sync(SyncMessage::SyncStep1(_)) => {}
            other => panic!("unexpected decode: {other:?}"),
        }

        // An awareness frame carries messageType=MSG_AWARENESS(1).
        let a = Awareness::new(Doc::new());
        a.set_local_state(serde_json::json!({"user":{"name":"x"}}))
            .unwrap();
        let awareness_frame = Message::Awareness(a.update().unwrap()).encode_v1();
        assert_eq!(awareness_frame[0], yrs::sync::protocol::MSG_AWARENESS);
    }
}
