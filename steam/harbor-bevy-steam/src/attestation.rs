//! Bridge Steam Web API tickets to an application-owned broker exchange.

use bevy::prelude::*;
use platform_attestation::SteamTicket;

/// Identity expected by the service that verifies these tickets.
#[derive(Debug, Default, Resource)]
pub struct SteamAttestationConfig {
    pub identity: String,
}

#[derive(Debug, Message)]
pub struct SteamTicketReady {
    #[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
    pub handle: bevy_steamworks::AuthTicket,
    pub ticket: SteamTicket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SteamAssertionError {
    #[error("Steam is unavailable")]
    Unavailable,
    #[error("Steam denied the ticket request or returned an invalid ticket")]
    Denied,
    #[error("Steam ticket identity must not be blank or contain NUL")]
    InvalidIdentity,
}

/// This plugin does not initialize Steam or contact the verifying service.
pub struct SteamAttestationPlugin;

impl Plugin for SteamAttestationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SteamAttestationConfig>()
            .add_message::<SteamTicketReady>();
        #[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
        steam::register(app);
    }
}

/// Clear the bytes held by this caller. Copies, including the SDK callback,
/// are not affected.
pub fn zero_ticket(ticket: &mut SteamTicket) {
    ticket.bytes.fill(0);
    std::hint::black_box(&ticket.bytes);
}

/// A successful local callback does not establish server-side attestation.
pub fn accepted<E>(result: &Result<(), E>) -> bool {
    result.is_ok()
}

#[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
mod steam {
    use super::*;
    use bevy_steamworks::{AuthTicket, CallbackResult, Client, SteamworksEvent};

    /// Correlates callbacks with requests made by this bridge and snapshots
    /// each identity at request time, even if the configuration later changes.
    #[derive(Default, Resource)]
    pub struct PendingSteamTickets(Vec<(AuthTicket, String)>);

    #[derive(Debug, Message)]
    pub struct SteamTicketFailed {
        pub handle: AuthTicket,
        pub error: SteamAssertionError,
    }

    pub(super) fn register(app: &mut App) {
        app.init_resource::<PendingSteamTickets>()
            .add_message::<SteamTicketFailed>()
            .add_systems(
                Update,
                poll_ticket_events
                    .run_if(resource_exists::<Client>)
                    .run_if(resource_exists::<Messages<SteamworksEvent>>),
            );
    }

    /// Start an asynchronous Steam ticket request. Read [`SteamTicketReady`]
    /// or [`SteamTicketFailed`] for the matching callback.
    pub fn request_webapi_ticket(
        client: Option<&Client>,
        config: &SteamAttestationConfig,
        pending: &mut PendingSteamTickets,
    ) -> Result<AuthTicket, SteamAssertionError> {
        let client = client.ok_or(SteamAssertionError::Unavailable)?;
        if config.identity.trim().is_empty() || config.identity.contains('\0') {
            return Err(SteamAssertionError::InvalidIdentity);
        }
        let user = client.user();
        if !user.logged_on() {
            return Err(SteamAssertionError::Unavailable);
        }
        let handle = user.authentication_session_ticket_for_webapi(&config.identity);
        pending.0.push((handle, config.identity.clone()));
        Ok(handle)
    }

    pub fn poll_ticket_events(
        mut events: MessageReader<SteamworksEvent>,
        mut ready: MessageWriter<SteamTicketReady>,
        mut failed: MessageWriter<SteamTicketFailed>,
        mut pending: ResMut<PendingSteamTickets>,
        client: Res<Client>,
    ) {
        for SteamworksEvent::CallbackResult(callback) in events.read() {
            let CallbackResult::TicketForWebApiResponse(response) = callback else {
                continue;
            };
            let handle = response.ticket_handle;
            let Some(index) = pending.0.iter().position(|(ticket, _)| *ticket == handle) else {
                continue;
            };
            let (_, identity) = pending.0.swap_remove(index);
            if let Some(ticket) = ticket_from_callback(
                accepted(&response.result),
                response.ticket_len,
                &response.ticket,
                identity,
            ) {
                ready.write(SteamTicketReady { handle, ticket });
            } else {
                client.user().cancel_authentication_ticket(handle);
                failed.write(SteamTicketFailed {
                    handle,
                    error: SteamAssertionError::Denied,
                });
            }
        }
    }

    fn ticket_from_callback(
        success: bool,
        len: i32,
        bytes: &[u8],
        identity: String,
    ) -> Option<SteamTicket> {
        let len = usize::try_from(len).ok()?;
        if !success || len == 0 {
            return None;
        }
        Some(SteamTicket {
            bytes: bytes.get(..len)?.to_vec(),
            identity_hint: Some(identity),
        })
    }

    /// Cancel and clear a ticket once the exchange reaches a terminal outcome.
    /// Cancelling while server-side verification is still pending may invalidate it.
    pub fn finish_ticket(client: &Client, handle: AuthTicket, ticket: &mut SteamTicket) {
        client.user().cancel_authentication_ticket(handle);
        zero_ticket(ticket);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn malformed_or_denied_callbacks_never_yield_tickets() {
            for (success, len) in [(false, 2), (true, -1), (true, 0), (true, 4)] {
                assert!(ticket_from_callback(success, len, &[1, 2, 3], "broker".into()).is_none());
            }
            let ticket = ticket_from_callback(true, 2, &[1, 2, 3], "broker".into()).unwrap();
            assert_eq!(ticket.bytes, [1, 2]);
            assert_eq!(ticket.identity_hint.as_deref(), Some("broker"));
        }

        #[test]
        fn absent_client_never_enqueues_a_ticket() {
            let mut pending = PendingSteamTickets::default();
            let config = SteamAttestationConfig {
                identity: "broker".into(),
            };
            assert_eq!(
                request_webapi_ticket(None, &config, &mut pending),
                Err(SteamAssertionError::Unavailable)
            );
            assert!(pending.0.is_empty());
        }
    }
}

#[cfg(all(feature = "steam", not(any(target_os = "android", target_os = "ios"))))]
pub use steam::{
    PendingSteamTickets, SteamTicketFailed, finish_ticket, poll_ticket_events,
    request_webapi_ticket,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_runs_without_steam_and_ticket_bytes_are_redacted_and_clearable() {
        let mut app = App::new();
        app.add_plugins(SteamAttestationPlugin);
        app.update();
        assert!(
            app.world()
                .resource::<Messages<SteamTicketReady>>()
                .is_empty()
        );
        assert!(accepted(&Ok::<_, SteamAssertionError>(())));
        assert!(!accepted(&Err(SteamAssertionError::Denied)));

        let mut ticket = SteamTicket {
            bytes: vec![17, 29, 43],
            identity_hint: None,
        };
        assert!(!format!("{ticket:?}").contains("17, 29, 43"));
        zero_ticket(&mut ticket);
        assert_eq!(ticket.bytes, [0, 0, 0]);
    }
}
