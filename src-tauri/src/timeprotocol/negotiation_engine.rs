// Phase-3 negotiation state engine.
//
// Pure function: takes the current `Negotiation` (or `None` for a fresh
// `request`) plus an envelope plus context (direction, own_email,
// source_message_id) and returns the updated `Negotiation` together
// with the `NegotiationMessage` row to persist.
//
// State transitions per `spec/v0.1.md` §5 + the v0.1-MVP-profile §4.2:
//
//   request          → state=Requested, no slots
//   propose          → state=Proposed, append slot
//   counter_propose  → state=Proposed, append slot (same shape as propose)
//   confirm          → state=Confirmed, mark referenced slot=Confirmed,
//                       all other Active slots → Inactive
//   release(slot_id) → mark referenced slot=Released; if no Active slots
//                       remain, state=Released
//   hold             → informational only, no state change
//
// Terminal-state guard: if the existing negotiation is already in
// Confirmed/Released/Expired, we reject the envelope per spec
// §"Minimal lifecycle rules" ("After a negotiation reaches confirmed,
// released, or expired, later actions for that negotiation must be
// rejected or ignored as stale").

use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use super::domain::{
    Envelope, MessageDirection, Negotiation, NegotiationAction,
    NegotiationConstraints, NegotiationMessage, NegotiationSlot,
    NegotiationState, SlotStatus, ThreadRole,
};

/// Apply an envelope to the (possibly absent) existing negotiation,
/// returning the updated state plus the message-log entry to persist.
///
/// The function is pure — no I/O, no DB. The caller is responsible for
/// idempotency (skipping already-seen `message_id`s) and for
/// persisting the result via `WriteCmd::ApplyNegotiationUpdate`.
pub fn apply_envelope(
    existing: Option<&Negotiation>,
    envelope: &Envelope,
    direction: MessageDirection,
    own_email: &str,
    source_message_id: Option<String>,
) -> Result<(Negotiation, NegotiationMessage), String> {
    // Terminal-state guard before anything else.
    if let Some(neg) = existing {
        if neg.state.is_terminal() {
            return Err(format!(
                "negotiation {} is already in terminal state {:?}; ignoring stale envelope",
                envelope.negotiation_id, neg.state
            ));
        }
    }

    // For each action, compute the new negotiation. Branches that
    // depend on an existing thread reject early when `existing` is
    // None — only `request` may bootstrap a fresh thread.
    let mut updated = match envelope.action {
        NegotiationAction::Request => apply_request(existing, envelope, direction, own_email)?,
        NegotiationAction::Propose | NegotiationAction::CounterPropose => {
            apply_propose(existing, envelope)?
        }
        NegotiationAction::Confirm => apply_confirm(existing, envelope)?,
        NegotiationAction::Release => apply_release(existing, envelope)?,
        NegotiationAction::Hold => apply_hold(existing, envelope)?,
    };

    updated.updated_at = Utc::now();

    let message = NegotiationMessage {
        message_id: envelope.message_id.clone(),
        direction,
        action: envelope.action,
        envelope: serde_json::to_value(envelope).map_err(|e| format!("envelope→value: {e}"))?,
        source_message_id,
        received_at: Utc::now(),
    };

    Ok((updated, message))
}

// ─── Per-action helpers ───────────────────────────────────────────────────

fn apply_request(
    existing: Option<&Negotiation>,
    envelope: &Envelope,
    direction: MessageDirection,
    own_email: &str,
) -> Result<Negotiation, String> {
    // Bootstrap path: a `request` may arrive against either a fresh
    // negotiation_id (most common) or against an existing one (rare —
    // a sender re-issues the same request after a transport failure;
    // we treat it idempotently if the existing thread is non-terminal).
    let (counterparty, thread_role) = match direction {
        MessageDirection::Inbound => {
            (envelope.from.clone(), ThreadRole::Responder)
        }
        MessageDirection::Outbound => {
            // Outbound request: we're the initiator, the counterparty
            // is the recipient address.
            (envelope.to.clone(), ThreadRole::Initiator)
        }
    };

    // Sanity-check that the address-of-the-other-side isn't us. We
    // don't fail on it (aliases muddy the picture) but log so debug
    // sessions notice when an envelope round-trips weirdly.
    if counterparty.eq_ignore_ascii_case(own_email) {
        tracing::warn!(
            counterparty = %counterparty,
            own_email = %own_email,
            "envelope counterparty matches our own email — alias confusion?"
        );
    }

    let payload: RequestPayload = serde_json::from_value(envelope.payload.clone())
        .map_err(|e| format!("request payload: {e}"))?;

    let now = Utc::now();
    let base = match existing {
        Some(n) => n.clone(),
        None => Negotiation {
            id: Uuid::new_v4().to_string(),
            negotiation_id: envelope.negotiation_id.clone(),
            thread_role,
            state: NegotiationState::Requested,
            duration_iso: Some(payload.duration.clone()),
            constraints: payload.constraints.clone(),
            counterparty_email: counterparty.clone(),
            counterparty_name: None,
            confirmed_commitment_id: None,
            display_summary: payload.summary.clone(),
            slots: Vec::new(),
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
        },
    };
    Ok(base)
}

fn apply_propose(
    existing: Option<&Negotiation>,
    envelope: &Envelope,
) -> Result<Negotiation, String> {
    let neg = existing.ok_or_else(|| {
        format!(
            "propose without prior request for negotiation {}",
            envelope.negotiation_id
        )
    })?;

    let payload: ProposePayload = serde_json::from_value(envelope.payload.clone())
        .map_err(|e| format!("propose payload: {e}"))?;

    // Idempotency on slot_id within a thread is enforced by the store's
    // UNIQUE(negotiation_id, slot_id) constraint. The engine still
    // de-duplicates here so the resulting in-memory `slots` vec is
    // consistent — the store will see the same vec the engine produced.
    let mut updated = neg.clone();
    if !updated.slots.iter().any(|s| s.slot_id == payload.slot_id) {
        updated.slots.push(NegotiationSlot {
            slot_id: payload.slot_id.clone(),
            proposer_node_id: envelope.from.clone(),
            start_at: payload.start_at,
            end_at: payload.end_at,
            status: SlotStatus::Active,
            proposed_at: Utc::now(),
        });
    }
    updated.state = NegotiationState::Proposed;
    Ok(updated)
}

fn apply_confirm(
    existing: Option<&Negotiation>,
    envelope: &Envelope,
) -> Result<Negotiation, String> {
    let neg = existing.ok_or_else(|| {
        format!(
            "confirm without prior request/propose for negotiation {}",
            envelope.negotiation_id
        )
    })?;
    let payload: SlotRefPayload = serde_json::from_value(envelope.payload.clone())
        .map_err(|e| format!("confirm payload: {e}"))?;

    // The referenced slot must currently be Active (or Held — held is
    // optional in the wire, so treat it as a stronger Active for our
    // purposes). Otherwise the spec says reject with `conflict`.
    let mut updated = neg.clone();
    let target = updated
        .slots
        .iter()
        .find(|s| s.slot_id == payload.slot_id)
        .ok_or_else(|| {
            format!(
                "confirm references unknown slot_id {} in negotiation {}",
                payload.slot_id, envelope.negotiation_id
            )
        })?;
    if !matches!(target.status, SlotStatus::Active) {
        return Err(format!(
            "confirm references slot {} which is in status {:?}; expected Active",
            payload.slot_id, target.status
        ));
    }

    // Mark the chosen slot Confirmed, all other Active ones Inactive.
    for slot in updated.slots.iter_mut() {
        if slot.slot_id == payload.slot_id {
            slot.status = SlotStatus::Confirmed;
        } else if matches!(slot.status, SlotStatus::Active) {
            slot.status = SlotStatus::Inactive;
        }
    }
    updated.state = NegotiationState::Confirmed;
    Ok(updated)
}

fn apply_release(
    existing: Option<&Negotiation>,
    envelope: &Envelope,
) -> Result<Negotiation, String> {
    let neg = existing.ok_or_else(|| {
        format!(
            "release without prior request/propose for negotiation {}",
            envelope.negotiation_id
        )
    })?;
    let payload: SlotRefPayload = serde_json::from_value(envelope.payload.clone())
        .map_err(|e| format!("release payload: {e}"))?;

    let mut updated = neg.clone();
    let mut found = false;
    for slot in updated.slots.iter_mut() {
        if slot.slot_id == payload.slot_id {
            slot.status = SlotStatus::Released;
            found = true;
        }
    }
    if !found {
        return Err(format!(
            "release references unknown slot_id {} in negotiation {}",
            payload.slot_id, envelope.negotiation_id
        ));
    }

    let any_active = updated
        .slots
        .iter()
        .any(|s| matches!(s.status, SlotStatus::Active));
    if !any_active {
        // Spec §5 "release_slot enters released for that slot_id and
        // ends that negotiation thread for v0.1." — once no active
        // proposals remain, the thread itself is released.
        updated.state = NegotiationState::Released;
    }
    Ok(updated)
}

fn apply_hold(
    existing: Option<&Negotiation>,
    envelope: &Envelope,
) -> Result<Negotiation, String> {
    // Optional in wire; informational only. Recipient is not required
    // to mirror it (MVP profile §4.1). We just acknowledge by
    // returning the existing state unchanged.
    existing
        .cloned()
        .ok_or_else(|| {
            format!(
                "hold without prior negotiation {}",
                envelope.negotiation_id
            )
        })
}

// ─── Payload types ────────────────────────────────────────────────────────
//
// We deserialise these on demand from `envelope.payload` rather than
// embedding them in the `Envelope` struct directly — keeps the wire
// type loose enough for forward-compatible payload extensions.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestPayload {
    duration: String,
    #[serde(default)]
    constraints: Option<NegotiationConstraints>,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProposePayload {
    slot_id: String,
    start_at: String,
    end_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlotRefPayload {
    slot_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req_envelope() -> Envelope {
        Envelope {
            message_id: "alice@example.com:msg-1".into(),
            from: "alice@example.com".into(),
            to: "bob@example.com".into(),
            negotiation_id: "alice@example.com:neg-1".into(),
            action: NegotiationAction::Request,
            timestamp: "2026-04-23T08:00:00+02:00".into(),
            payload: json!({
                "duration": "PT45M",
                "summary": "Sync"
            }),
        }
    }

    #[test]
    fn fresh_inbound_request_creates_responder_thread() {
        let (neg, msg) =
            apply_envelope(None, &req_envelope(), MessageDirection::Inbound, "bob@example.com", None)
                .expect("ok");
        assert_eq!(neg.thread_role, ThreadRole::Responder);
        assert_eq!(neg.state, NegotiationState::Requested);
        assert_eq!(neg.counterparty_email, "alice@example.com");
        assert_eq!(neg.duration_iso.as_deref(), Some("PT45M"));
        assert_eq!(msg.action, NegotiationAction::Request);
        assert_eq!(msg.direction, MessageDirection::Inbound);
    }

    #[test]
    fn outbound_request_creates_initiator_thread() {
        let (neg, _) = apply_envelope(
            None,
            &Envelope {
                from: "bob@example.com".into(),
                to: "alice@example.com".into(),
                ..req_envelope()
            },
            MessageDirection::Outbound,
            "bob@example.com",
            None,
        )
        .expect("ok");
        assert_eq!(neg.thread_role, ThreadRole::Initiator);
        assert_eq!(neg.counterparty_email, "alice@example.com");
    }

    #[test]
    fn propose_against_no_existing_fails() {
        let env = Envelope {
            action: NegotiationAction::Propose,
            payload: json!({
                "slotId": "x:1",
                "startAt": "2026-04-23T09:00:00Z",
                "endAt": "2026-04-23T09:45:00Z"
            }),
            ..req_envelope()
        };
        assert!(apply_envelope(None, &env, MessageDirection::Inbound, "x@y", None).is_err());
    }

    #[test]
    fn propose_appends_slot_and_advances_state() {
        let (initial, _) =
            apply_envelope(None, &req_envelope(), MessageDirection::Inbound, "bob@example.com", None)
                .expect("ok");
        let env = Envelope {
            message_id: "bob@example.com:msg-2".into(),
            from: "bob@example.com".into(),
            to: "alice@example.com".into(),
            action: NegotiationAction::Propose,
            payload: json!({
                "slotId": "bob@example.com:slot-1",
                "startAt": "2026-04-23T09:00:00Z",
                "endAt": "2026-04-23T09:45:00Z"
            }),
            ..req_envelope()
        };
        let (updated, _) = apply_envelope(
            Some(&initial),
            &env,
            MessageDirection::Outbound,
            "bob@example.com",
            None,
        )
        .expect("ok");
        assert_eq!(updated.state, NegotiationState::Proposed);
        assert_eq!(updated.slots.len(), 1);
        assert_eq!(updated.slots[0].slot_id, "bob@example.com:slot-1");
        assert_eq!(updated.slots[0].status, SlotStatus::Active);
    }

    #[test]
    fn confirm_marks_target_and_deactivates_others() {
        let (initial, _) =
            apply_envelope(None, &req_envelope(), MessageDirection::Inbound, "bob@example.com", None)
                .expect("ok");
        let mut after_propose = initial.clone();
        // Manually inject two slots so we can test the deactivation path.
        after_propose.slots.push(NegotiationSlot {
            slot_id: "bob@example.com:slot-1".into(),
            proposer_node_id: "bob@example.com".into(),
            start_at: "2026-04-23T09:00:00Z".into(),
            end_at: "2026-04-23T09:45:00Z".into(),
            status: SlotStatus::Active,
            proposed_at: Utc::now(),
        });
        after_propose.slots.push(NegotiationSlot {
            slot_id: "bob@example.com:slot-2".into(),
            proposer_node_id: "bob@example.com".into(),
            start_at: "2026-04-23T14:00:00Z".into(),
            end_at: "2026-04-23T14:45:00Z".into(),
            status: SlotStatus::Active,
            proposed_at: Utc::now(),
        });
        after_propose.state = NegotiationState::Proposed;

        let env = Envelope {
            message_id: "alice@example.com:msg-3".into(),
            action: NegotiationAction::Confirm,
            payload: json!({ "slotId": "bob@example.com:slot-1" }),
            ..req_envelope()
        };
        let (updated, _) = apply_envelope(
            Some(&after_propose),
            &env,
            MessageDirection::Inbound,
            "bob@example.com",
            None,
        )
        .expect("ok");
        assert_eq!(updated.state, NegotiationState::Confirmed);
        assert_eq!(
            updated.slots.iter().find(|s| s.slot_id == "bob@example.com:slot-1").unwrap().status,
            SlotStatus::Confirmed
        );
        assert_eq!(
            updated.slots.iter().find(|s| s.slot_id == "bob@example.com:slot-2").unwrap().status,
            SlotStatus::Inactive
        );
    }

    #[test]
    fn confirm_unknown_slot_fails() {
        let mut neg = apply_envelope(None, &req_envelope(), MessageDirection::Inbound, "bob@example.com", None)
            .unwrap()
            .0;
        neg.state = NegotiationState::Proposed;
        let env = Envelope {
            action: NegotiationAction::Confirm,
            payload: json!({ "slotId": "non-existent:slot" }),
            ..req_envelope()
        };
        assert!(apply_envelope(Some(&neg), &env, MessageDirection::Inbound, "bob@example.com", None).is_err());
    }

    #[test]
    fn release_last_active_slot_terminates_thread() {
        let (initial, _) =
            apply_envelope(None, &req_envelope(), MessageDirection::Inbound, "bob@example.com", None)
                .unwrap();
        let mut neg = initial;
        neg.slots.push(NegotiationSlot {
            slot_id: "bob@example.com:only".into(),
            proposer_node_id: "bob@example.com".into(),
            start_at: "2026-04-23T09:00:00Z".into(),
            end_at: "2026-04-23T09:45:00Z".into(),
            status: SlotStatus::Active,
            proposed_at: Utc::now(),
        });
        neg.state = NegotiationState::Proposed;

        let env = Envelope {
            action: NegotiationAction::Release,
            payload: json!({ "slotId": "bob@example.com:only" }),
            ..req_envelope()
        };
        let (updated, _) = apply_envelope(Some(&neg), &env, MessageDirection::Inbound, "bob@example.com", None).unwrap();
        assert_eq!(updated.state, NegotiationState::Released);
        assert_eq!(updated.slots[0].status, SlotStatus::Released);
    }

    #[test]
    fn terminal_state_rejects_further_action() {
        let mut neg = apply_envelope(None, &req_envelope(), MessageDirection::Inbound, "bob@example.com", None)
            .unwrap()
            .0;
        neg.state = NegotiationState::Confirmed;
        let env = Envelope {
            action: NegotiationAction::Propose,
            payload: json!({
                "slotId": "x:1",
                "startAt": "2026-04-23T09:00:00Z",
                "endAt": "2026-04-23T09:45:00Z"
            }),
            ..req_envelope()
        };
        assert!(apply_envelope(Some(&neg), &env, MessageDirection::Inbound, "bob@example.com", None).is_err());
    }
}
