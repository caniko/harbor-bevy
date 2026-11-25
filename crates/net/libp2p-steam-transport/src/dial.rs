use std::sync::Arc;
use std::time::Duration;

use steamworks::networking_sockets::{NetConnection, NetworkingSockets};
use tokio::sync::oneshot;

use crate::error::SteamTransportError;

/// Spawn a blocking task that polls connection state at 16ms intervals until
/// the connection is established or fails.
///
/// The `DialFuture` returned by `Transport::dial()` awaits the oneshot receiver,
/// consuming 0 CPU until this task completes it.
///
/// We poll rather than use callbacks because the steamworks crate's
/// `independent_connection_callback` is unimplemented (TODO in
/// `networking_sockets_callback.rs`). The 16ms interval aligns with a 60Hz
/// game tick. This task is short-lived (~1s for typical P2P connection).
pub(crate) fn spawn_dial(
    sockets: Arc<NetworkingSockets>,
    connection: NetConnection,
) -> oneshot::Receiver<Result<NetConnection, SteamTransportError>> {
    let (tx, rx) = oneshot::channel();

    tokio::task::spawn_blocking(move || {
        loop {
            match sockets.get_connection_info(&connection) {
                Ok(info) => match info.state().map(|state| format!("{state:?}")) {
                    Ok(state) if state == "Connected" => {
                        log::debug!("Steam dial: connection established");
                        let _ = tx.send(Ok(connection));
                        return;
                    }
                    Ok(state) if state == "ClosedByPeer" => {
                        log::debug!("Steam dial: closed by peer");
                        let _ = tx.send(Err(SteamTransportError::ConnectFailed(
                            "closed by peer".into(),
                        )));
                        return;
                    }
                    Ok(state) if state == "ProblemDetectedLocally" => {
                        log::debug!("Steam dial: problem detected locally");
                        let _ = tx.send(Err(SteamTransportError::ConnectFailed(
                            "problem detected locally".into(),
                        )));
                        return;
                    }
                    Ok(state) if state == "None" => {
                        log::debug!("Steam dial: connection state None (dead)");
                        let _ = tx.send(Err(SteamTransportError::ConnectFailed(
                            "connection dead".into(),
                        )));
                        return;
                    }
                    Ok(_) => {
                        // Still connecting (Connecting, FindingRoute, etc.)
                    }
                    Err(_) => {
                        let _ = tx.send(Err(SteamTransportError::ConnectFailed(
                            "invalid connection state".into(),
                        )));
                        return;
                    }
                },
                Err(_) => {
                    let _ = tx.send(Err(SteamTransportError::ConnectFailed(
                        "invalid connection handle".into(),
                    )));
                    return;
                }
            }

            // Check if the receiver was dropped (dial cancelled)
            if tx.is_closed() {
                log::debug!("Steam dial: cancelled by caller");
                return;
            }

            std::thread::sleep(Duration::from_millis(16));
        }
    });

    rx
}
