// CrystalMail × timeProtocol — Calendar bounded context.
//
// This module owns all calendar / iCalendar concerns for CrystalMail:
// the local domain types, ICS parsing/building (RFC 5545), the persistence
// layer for stored commitments (Phase 1+), and the Tauri command surface
// the frontend's Calendar features call into.
//
// Architecture boundary (decided 2026-05-06):
//   1. **Inbound** — reading bytes of a stored mail attachment (e.g. an
//      incoming `text/calendar` invite). Currently realised via
//      `application::attachments::bytes`. This is the *one* read access
//      this module makes into the Mail layer.
//   2. **Outbound** — a REPLY ICS is dropped onto disk and surfaced to the
//      frontend, which builds a regular `ComposeDraft` around it. The
//      existing SMTP path (`application::smtp::is_imip_attachment` +
//      `build_imip_alternative`) recognises the `text/calendar; method=`
//      attachment and emits an iMIP-compliant message. No direct send
//      invocation from this module.
//
// Anything beyond those two contact points requires a deliberate
// refactor of this boundary, not a drive-by addition. If a future feature
// needs (say) Mail-level threading data or full RFC822 access, surface it
// as a thin facade in `mail` and let `timeprotocol` consume that facade.

pub mod commands;
pub mod domain;
pub mod ics;
pub mod store;
