//! Mirror optional-channel health without polling or blocking native callbacks.

use std::sync::{Arc, Mutex};

use super::ServerEventTx;
use crate::gfx::GfxState;

pub(super) async fn mirror(sender: ServerEventTx, state: Arc<Mutex<GfxState>>) {
    sender.clipboard_quarantined().await;
    state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clipboard_disabled = true;
    // The connection loop owns this future. Keep it pending after publishing
    // the sticky status so channel failure cannot terminate that loop.
    std::future::pending::<()>().await;
}
