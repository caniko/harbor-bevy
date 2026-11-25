//! Frame dispatch — pure logic, swarm-agnostic.

use crate::store::Store;
use rkyv::api::high::HighValidator;
use rkyv::bytecheck::CheckBytes;
use rkyv::rancor;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use thespan_rendezvous_proto::{
    DiscoverRequest, DiscoverResponse, ErrorCode, Frame, HeartbeatRequest, LobbyEntry,
    MAX_DISCOVER_LIMIT, MAX_REGISTRATION_TTL_SECS, ProtoError, RegisterAck, RegisterRequest,
    SignedEnvelope, UnregisterRequest,
};
use tracing::warn;

pub struct Handler {
    pub store: Arc<Store>,
    pub clock_skew: Duration,
    pub replay_window: Duration,
}

impl Handler {
    pub fn handle(&self, req: Frame) -> Frame {
        match req {
            Frame::Register(env) => self.on_register(env),
            Frame::Unregister(env) => self.on_unregister(env),
            Frame::Heartbeat(env) => self.on_heartbeat(env),
            Frame::Discover(req) => self.on_discover(req),
            // Responses arriving as a request are nonsense.
            Frame::RegisterAck(_) | Frame::DiscoverResponse(_) | Frame::Error(_) => Frame::Error(
                ProtoError::new(ErrorCode::Malformed, "response frame on inbound stream"),
            ),
        }
    }

    fn check_envelope<T>(&self, env: &SignedEnvelope) -> Result<T, ProtoError>
    where
        T: rkyv::Archive,
        T::Archived: rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rancor::Error>>
            + for<'a> CheckBytes<HighValidator<'a, rancor::Error>>,
    {
        let _pk = crate::verify::verify(env, SystemTime::now(), self.clock_skew)?;
        if !self
            .store
            .check_and_record_nonce(&env.signer_peer_id, env.nonce, self.replay_window)
        {
            return Err(ProtoError::new(ErrorCode::Replayed, "nonce reused"));
        }
        rkyv::from_bytes::<T, rancor::Error>(&env.payload)
            .map_err(|e| ProtoError::new(ErrorCode::Malformed, format!("payload decode: {e}")))
    }

    fn on_register(&self, env: SignedEnvelope) -> Frame {
        let req: RegisterRequest = match self.check_envelope(&env) {
            Ok(r) => r,
            Err(e) => return Frame::Error(e),
        };
        // Authorisation: the announced peer in the ticket must equal the signer.
        if req.ticket.peer_id.0 != env.signer_peer_id.0 {
            return Frame::Error(ProtoError::new(
                ErrorCode::AuthFailed,
                "ticket.peer_id != signer_peer_id",
            ));
        }
        let ttl = Duration::from_secs(req.ttl_secs.min(MAX_REGISTRATION_TTL_SECS) as u64);
        match self.store.upsert(
            req.namespace.into_inner(),
            env.signer_peer_id,
            req.ticket,
            req.metadata,
            ttl,
        ) {
            Some(id) => Frame::RegisterAck(RegisterAck {
                granted_ttl_secs: ttl.as_secs() as u32,
                registration_id: id,
            }),
            None => {
                warn!("registration store at capacity");
                Frame::Error(ProtoError::new(ErrorCode::Unavailable, "store at capacity"))
            }
        }
    }

    fn on_unregister(&self, env: SignedEnvelope) -> Frame {
        let req: UnregisterRequest = match self.check_envelope(&env) {
            Ok(r) => r,
            Err(e) => return Frame::Error(e),
        };
        self.store
            .unregister(req.namespace.as_str(), &env.signer_peer_id);
        Frame::RegisterAck(RegisterAck {
            granted_ttl_secs: 0,
            registration_id: 0,
        })
    }

    fn on_heartbeat(&self, env: SignedEnvelope) -> Frame {
        let req: HeartbeatRequest = match self.check_envelope(&env) {
            Ok(r) => r,
            Err(e) => return Frame::Error(e),
        };
        let ttl = Duration::from_secs(req.ttl_secs.min(MAX_REGISTRATION_TTL_SECS) as u64);
        match self
            .store
            .heartbeat(req.namespace.as_str(), &env.signer_peer_id, ttl)
        {
            Some(id) => Frame::RegisterAck(RegisterAck {
                granted_ttl_secs: ttl.as_secs() as u32,
                registration_id: id,
            }),
            None => Frame::Error(ProtoError::new(ErrorCode::NotFound, "no live registration")),
        }
    }

    fn on_discover(&self, req: DiscoverRequest) -> Frame {
        let limit = req.limit.min(MAX_DISCOVER_LIMIT) as usize;
        let (rows, next) = self
            .store
            .discover(req.namespace.as_str(), &req.cursor, limit);
        let now = Instant::now();
        let entries = rows
            .into_iter()
            .map(|r| LobbyEntry {
                ticket: r.ticket,
                metadata: r.metadata,
                ttl_remaining_secs: r.expires_at.saturating_duration_since(now).as_secs() as u32,
            })
            .collect();
        Frame::DiscoverResponse(DiscoverResponse {
            entries,
            next_cursor: next,
        })
    }
}
