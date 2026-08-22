use std::time::Duration;

use axum::{
    extract::{Query, State, WebSocketUpgrade, ws::Message},
    response::Response,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::broadcast, time};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    state::{AccessTokenAuthentication, AppState, AuthPrincipal, EffectiveAccess},
};

const CHANNEL_CAPACITY: usize = 512;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct RealtimePublication {
    pub tenant_id: String,
    pub user_id: Option<String>,
    pub required_permission: Option<String>,
    pub event_type: String,
    pub data: Value,
}

impl RealtimePublication {
    pub fn tenant(
        tenant_id: impl Into<String>,
        event_type: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: None,
            required_permission: None,
            event_type: event_type.into(),
            data,
        }
    }

    pub fn for_user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RealtimeEnvelope {
    id: Uuid,
    #[serde(rename = "type")]
    event_type: String,
    version: u8,
    occurred_at: DateTime<Utc>,
    data: Value,
}

impl RealtimeEnvelope {
    fn new(event_type: impl Into<String>, data: Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_type: event_type.into(),
            version: 1,
            occurred_at: Utc::now(),
            data,
        }
    }
}

#[derive(Clone)]
pub struct RealtimeHub {
    sender: broadcast::Sender<RealtimePublication>,
}

impl Default for RealtimeHub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { sender }
    }
}

impl RealtimeHub {
    pub fn subscribe(&self) -> broadcast::Receiver<RealtimePublication> {
        self.sender.subscribe()
    }

    pub fn publish(&self, publication: RealtimePublication) {
        // Having no connected clients is normal and is not an error.
        let _ = self.sender.send(publication);
    }
}

#[derive(Debug, Deserialize)]
pub struct RealtimeTicketQuery {
    access_token: Option<String>,
}

pub async fn websocket(
    State(state): State<AppState>,
    Query(query): Query<RealtimeTicketQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let token = query
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(ApiError::Unauthorized)?;
    let mut principal = match state.authenticate_access_token(token).await? {
        AccessTokenAuthentication::Authenticated(principal) => *principal,
        AccessTokenAuthentication::Expired => return Err(ApiError::AccessTokenExpired),
        AccessTokenAuthentication::Invalid => return Err(ApiError::InvalidAccessToken),
        AccessTokenAuthentication::SessionInactive => return Err(ApiError::SessionInactive),
    };

    // Resolve permissions again at connection time. A ticket never preserves access
    // that an administrator removed after it was issued.
    let access = state
        .effective_access_for_surface(&principal.student.tenant_id, &principal.student.id, "app")
        .await?;
    principal.roles = access.roles.clone();
    principal.student.role = access.roles.first().cloned().unwrap_or_default();
    principal.student.access = access.permissions.clone();

    Ok(ws.on_upgrade(move |socket| serve_socket(state, principal, access, socket)))
}

async fn serve_socket(
    state: AppState,
    principal: AuthPrincipal,
    access: EffectiveAccess,
    socket: axum::extract::ws::WebSocket,
) {
    let mut receiver = state.subscribe_realtime();
    let (mut sender, mut incoming) = socket.split();
    let mut heartbeat = time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    if send_envelope(
        &mut sender,
        RealtimeEnvelope::new(
            "realtime.ready",
            json!({"tenantId": principal.student.tenant_id, "userId": principal.student.id}),
        ),
    )
    .await
    .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            message = incoming.next() => match message {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Ping(payload))) => {
                    if sender.send(Message::Pong(payload)).await.is_err() { break; }
                }
                Some(Ok(_)) => {}
            },
            publication = receiver.recv() => match publication {
                Ok(publication) if visible_to(&publication, &principal, &access) => {
                    if send_envelope(
                        &mut sender,
                        RealtimeEnvelope::new(publication.event_type, publication.data),
                    ).await.is_err() { break; }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if send_envelope(
                        &mut sender,
                        RealtimeEnvelope::new("realtime.resync_required", json!({})),
                    ).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = heartbeat.tick() => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
}

fn visible_to(
    publication: &RealtimePublication,
    principal: &AuthPrincipal,
    access: &EffectiveAccess,
) -> bool {
    publication.tenant_id == principal.student.tenant_id
        && publication
            .user_id
            .as_deref()
            .is_none_or(|user_id| user_id == principal.student.id)
        && publication
            .required_permission
            .as_deref()
            .is_none_or(|permission| access.allows(permission))
}

async fn send_envelope(
    sender: &mut futures_util::stream::SplitSink<axum::extract::ws::WebSocket, Message>,
    envelope: RealtimeEnvelope,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(&envelope).unwrap_or_else(|_| {
        r#"{"type":"realtime.serialization_error","version":1,"data":{}}"#.into()
    });
    sender.send(Message::Text(text.into())).await
}
