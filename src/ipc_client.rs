//! Thin wrapper around `tetron_proto::ipc`'s connect/send/recv helpers.
//!
//! One connection per request, same convention `tetron-webui` and tetron's
//! own CLI both follow: a daemon restart is never something this process
//! has to detect and recover from, the *next* poll just reconnects fresh.

use tetron_proto::ipc::{self, IpcMessage};

/// Send one `IpcMessage` to the daemon and return its reply.
///
/// Transport-layer only -- does not unwrap `IpcMessage::Error` into an
/// `Err`, since "daemon understood and rejected the request" and "could not
/// reach the daemon at all" are different failure modes callers usually
/// want to render differently (e.g. the tray icon's own unreachable state
/// vs. a one-off action failing).
pub async fn call(msg: IpcMessage) -> Result<IpcMessage, String> {
    let mut stream = ipc::connect()
        .await
        .map_err(|e| format!("could not reach the tetron daemon: {e}"))?;
    ipc::send(&mut stream, msg)
        .await
        .map_err(|e| format!("failed to send request to daemon: {e}"))?;
    ipc::recv(&mut stream)
        .await
        .map_err(|e| format!("failed to read daemon response: {e}"))
}
