// CrystalMail × timeProtocol — Calendar bounded context.
//
// This module owns all calendar / iCalendar concerns for CrystalMail:
// the local domain types, ICS parsing/building (RFC 5545), the persistence
// layer for stored commitments (Phase 1+), and the Tauri command surface
// the frontend's Calendar features call into.
//
// Architecture boundary (decided 2026-05-06):
//   1. **Inbound** — reading bytes of a stored mail attachment (e.g. an
//      incoming `text/calendar` invite). Realised via
//      `application::attachments::bytes`. The Phase-2 IMAP-folder pull
//      goes through `application::calendar_imap` (a thin facade owned
//      by the Mail layer); this is still inbound — `timeprotocol` only
//      reads, never writes mail-domain state directly.
//   2. **Outbound** — Phase 0/1: a REPLY ICS is dropped onto disk and
//      surfaced to the frontend, which builds a regular `ComposeDraft`
//      around it. The existing SMTP path's iMIP detection
//      (`application::smtp::is_imip_attachment` + `build_imip_alternative`)
//      emits an iMIP-compliant message. Phase 2: the IMAP-folder publish
//      uses `application::calendar_imap::append_message`, which is the
//      only direct IMAP write surface this module touches.
//
// Anything beyond those contact points requires a deliberate refactor of
// this boundary, not a drive-by addition. If a future feature needs (say)
// Mail-level threading data or full RFC822 access from elsewhere, surface
// it as a thin facade in `mail` and let `timeprotocol` consume that facade.

pub mod commands;
pub mod domain;
pub mod ics;
pub mod store;
pub mod sync;
