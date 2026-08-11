use std::{
    io::{BufRead, BufReader},
    time::Duration,
};

use reqwest::{
    blocking::{Client, Response},
    header::{ACCEPT, CONTENT_TYPE},
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::error::{OpenCodeAdapterError, OpenCodeDecodeError};

/// Configuration for one OpenCode server connection.
#[derive(Debug, Clone)]
pub struct OpenCodeClientConfig {
    pub base_url: String,
    pub project_directory: Option<String>,
    pub request_timeout: Duration,
}

impl Default for OpenCodeClientConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:4096".to_owned(),
            project_directory: None,
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// A blocking REST + structured SSE OpenCode client.
#[derive(Debug, Clone)]
pub struct OpenCodeClient {
    config: OpenCodeClientConfig,
    http: Client,
}

impl OpenCodeClient {
    pub fn new(config: OpenCodeClientConfig) -> Result<Self, OpenCodeAdapterError> {
        let http = Client::builder().timeout(config.request_timeout).build()?;
        Ok(Self { config, http })
    }

    pub fn config(&self) -> &OpenCodeClientConfig {
        &self.config
    }

    /// Discover all sessions through the server's REST API.
    pub fn list_sessions(&self) -> Result<Vec<Value>, OpenCodeAdapterError> {
        let response = self.get("/session")?;
        let value: Value = response.json()?;
        validate_generated::<crate::wire::SessionListResponse>(&value)?;
        let sessions = value
            .as_array()
            .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
        sessions
            .iter()
            .cloned()
            .map(validate_session_value)
            .collect()
    }

    pub fn get_session(&self, session_id: &str) -> Result<Value, OpenCodeAdapterError> {
        let response = self.get(&format!("/session/{}", path_segment(session_id)?))?;
        let value: Value = response.json()?;
        validate_session_value(value)
    }

    pub fn get_messages(&self, session_id: &str) -> Result<Vec<Value>, OpenCodeAdapterError> {
        let response = self.get(&format!("/session/{}/message", path_segment(session_id)?))?;
        let value: Value = response.json()?;
        validate_generated::<crate::wire::SessionMessagesResponse>(&value)?;
        let messages = value
            .as_array()
            .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
        messages
            .iter()
            .cloned()
            .map(validate_message_value)
            .collect()
    }

    pub fn get_project_current(&self) -> Result<Value, OpenCodeAdapterError> {
        let response = self.get("/project/current")?;
        let value: Value = response.json()?;
        validate_project_scope(&value, self.config.project_directory.as_deref())?;
        Ok(value)
    }

    pub fn create_session(&self, title: Option<&str>) -> Result<Value, OpenCodeAdapterError> {
        let mut body = serde_json::Map::new();
        if let Some(title) = title {
            body.insert("title".to_owned(), Value::String(title.to_owned()));
        }
        let body = generated_body::<crate::wire::CreateSessionRequest>(Value::Object(body))?;
        let response = self.post_json("/session", body)?;
        let value = response.json()?;
        validate_session_value(value)
    }

    /// Submit a prompt and return the server's structured message response.
    pub fn prompt(&self, session_id: &str, text: &str) -> Result<Value, OpenCodeAdapterError> {
        let body = json!({
            "parts": [{"type": "text", "text": text}],
        });
        let body = generated_body::<crate::wire::PromptRequest>(body)?;
        let response = self.post_json(
            &format!("/session/{}/message", path_segment(session_id)?),
            body,
        )?;
        let value = response.json()?;
        validate_generated::<crate::wire::PromptResponse>(&value)?;
        Ok(value)
    }

    pub fn prompt_async(&self, session_id: &str, text: &str) -> Result<(), OpenCodeAdapterError> {
        let body = json!({
            "parts": [{"type": "text", "text": text}],
        });
        let body = generated_body::<crate::wire::PromptAsyncRequest>(body)?;
        let response = self.post_json(
            &format!("/session/{}/prompt_async", path_segment(session_id)?),
            body,
        )?;
        let _ = response;
        Ok(())
    }

    /// Durably admit a prompt through the v2 session route.  The response is
    /// decoded against the generated OpenAPI envelope and admission schema;
    /// callers must not infer acknowledgement from the legacy async route.
    pub fn prompt_v2(
        &self,
        session_id: &str,
        text: &str,
        delivery: PromptDelivery,
    ) -> Result<PromptAdmission, OpenCodeAdapterError> {
        let body = generated_body::<crate::wire::SessionPromptV2Request>(json!({
            "prompt": {"text": text},
            "delivery": delivery.as_wire(),
        }))?;
        let response = self.post_json(
            &format!("/api/session/{}/prompt", path_segment(session_id)?),
            body,
        )?;
        let value: Value = response.json()?;
        let envelope = serde_json::from_value::<crate::wire::SessionPromptV2Response>(value)
            .map_err(OpenCodeDecodeError::MalformedJson)?;
        decode_prompt_admission(envelope, session_id, None, delivery)
    }

    pub fn abort(&self, session_id: &str) -> Result<(), OpenCodeAdapterError> {
        let response = self.post_empty(&format!("/session/{}/abort", path_segment(session_id)?))?;
        require_true_response::<crate::wire::AbortSessionResponse>(response)
    }

    pub fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        reply: PermissionReply,
    ) -> Result<(), OpenCodeAdapterError> {
        let body = generated_body::<crate::wire::SessionPermissionReplyRequest>(
            json!({"response": reply.as_wire()}),
        )?;
        let path = format!(
            "/session/{}/permissions/{}/",
            path_segment(session_id)?,
            path_segment(request_id)?
        );
        let response = self.post_json(path.trim_end_matches('/'), body)?;
        require_true_response::<crate::wire::SessionPermissionReplyResponse>(response)
    }

    pub fn reply_permission_legacy(
        &self,
        request_id: &str,
        reply: PermissionReply,
    ) -> Result<(), OpenCodeAdapterError> {
        let body = generated_body::<crate::wire::PermissionReplyRequest>(
            json!({"reply": reply.as_wire()}),
        )?;
        let response = self.post_json(
            &format!("/permission/{}/reply", path_segment(request_id)?),
            body,
        )?;
        let _ = response;
        Ok(())
    }

    pub fn reply_permission_v2(
        &self,
        session_id: &str,
        request_id: &str,
        reply: PermissionReply,
    ) -> Result<(), OpenCodeAdapterError> {
        let body = generated_body::<crate::wire::PermissionV2ReplyRequest>(
            json!({"reply": reply.as_wire()}),
        )?;
        let response = self.post_json(
            &format!(
                "/api/session/{}/permission/{}/reply",
                path_segment(session_id)?,
                path_segment(request_id)?
            ),
            body,
        )?;
        let _ = response;
        Ok(())
    }

    pub fn reply_question(
        &self,
        session_id: &str,
        request_id: &str,
        answers: Vec<Vec<String>>,
    ) -> Result<(), OpenCodeAdapterError> {
        let body =
            generated_body::<crate::wire::QuestionV2ReplyRequest>(json!({"answers": answers}))?;
        let response = self.post_json(
            &format!(
                "/api/session/{}/question/{}/reply",
                path_segment(session_id)?,
                path_segment(request_id)?
            ),
            body,
        )?;
        let _ = response;
        Ok(())
    }

    pub fn reject_question(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<(), OpenCodeAdapterError> {
        let response = self.post_empty(&format!(
            "/api/session/{}/question/{}/reject",
            path_segment(session_id)?,
            path_segment(request_id)?
        ))?;
        let _ = response;
        Ok(())
    }

    /// Open the structured instance SSE stream.  OpenCode supplies no replay
    /// cursor in this endpoint; reconnect callers must take a fresh REST
    /// snapshot before subscribing again.
    pub fn subscribe(&self) -> Result<SseStream, OpenCodeAdapterError> {
        let mut request = self
            .http
            .get(self.url("/event"))
            .header(ACCEPT, "text/event-stream");
        if let Some(directory) = &self.config.project_directory {
            request = request.query(&[("directory", directory)]);
        }
        let response = checked(request.send()?)?;
        Ok(SseStream {
            reader: BufReader::new(response),
            pending_event: None,
            pending_data: Vec::new(),
            pending_id: None,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.base_url.trim_end_matches('/'), path)
    }

    fn get(&self, path: &str) -> Result<Response, OpenCodeAdapterError> {
        let mut request = self.http.get(self.url(path));
        if let Some(directory) = &self.config.project_directory {
            request = request.query(&[("directory", directory)]);
        }
        checked(request.send()?)
    }

    fn post_json(&self, path: &str, body: Value) -> Result<Response, OpenCodeAdapterError> {
        let mut request = self
            .http
            .post(self.url(path))
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(directory) = &self.config.project_directory {
            request = request.query(&[("directory", directory)]);
        }
        checked(request.send()?)
    }

    fn post_empty(&self, path: &str) -> Result<Response, OpenCodeAdapterError> {
        let mut request = self.http.post(self.url(path));
        if let Some(directory) = &self.config.project_directory {
            request = request.query(&[("directory", directory)]);
        }
        checked(request.send()?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReply {
    Once,
    Always,
    Reject,
}

/// Delivery mode accepted by OpenCode's durable v2 prompt endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDelivery {
    Steer,
    Queue,
}

impl PromptDelivery {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::Queue => "queue",
        }
    }
}

/// The server's durable admission receipt.  This is intentionally separate
/// from a shared command acknowledgement: it is only returned after the v2
/// response has passed generated-schema and scope validation.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptAdmission {
    pub admitted_seq: u64,
    pub id: String,
    pub session_id: String,
    pub prompt: String,
    pub delivery: PromptDelivery,
    pub time_created: f64,
    pub promoted_seq: Option<u64>,
}

fn decode_prompt_admission(
    envelope: crate::wire::SessionPromptV2Response,
    expected_session_id: &str,
    expected_message_id: Option<&str>,
    requested_delivery: PromptDelivery,
) -> Result<PromptAdmission, OpenCodeAdapterError> {
    let admitted = envelope.data;
    let session_id: String = admitted.session_id.into();
    if session_id != expected_session_id {
        return Err(OpenCodeDecodeError::ScopeMismatch.into());
    }
    let id: String = admitted.id.into();
    if expected_message_id.is_some_and(|expected| expected != id) {
        return Err(OpenCodeDecodeError::AdmissionMismatch("message id").into());
    }
    let delivery = match admitted.delivery {
        crate::wire::SessionInputAdmittedDelivery::Steer => PromptDelivery::Steer,
        crate::wire::SessionInputAdmittedDelivery::Queue => PromptDelivery::Queue,
    };
    if delivery != requested_delivery {
        return Err(OpenCodeDecodeError::AdmissionMismatch("delivery").into());
    }
    Ok(PromptAdmission {
        admitted_seq: admitted.admitted_seq,
        id,
        session_id,
        prompt: admitted.prompt.text,
        delivery,
        time_created: admitted.time_created,
        promoted_seq: admitted.promoted_seq,
    })
}

impl PermissionReply {
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Always => "always",
            Self::Reject => "reject",
        }
    }
}

fn generated_body<T>(value: Value) -> Result<Value, OpenCodeAdapterError>
where
    T: DeserializeOwned + Serialize,
{
    let typed = serde_json::from_value::<T>(value).map_err(OpenCodeDecodeError::MalformedJson)?;
    serde_json::to_value(typed)
        .map_err(OpenCodeDecodeError::MalformedJson)
        .map_err(Into::into)
}

fn validate_generated<T>(value: &Value) -> Result<(), OpenCodeAdapterError>
where
    T: DeserializeOwned,
{
    serde_json::from_value::<T>(value.clone())
        .map(|_| ())
        .map_err(OpenCodeDecodeError::MalformedJson)
        .map_err(Into::into)
}

fn require_true_response<T>(response: Response) -> Result<(), OpenCodeAdapterError>
where
    T: DeserializeOwned,
{
    let value: Value = response.json()?;
    require_true_value::<T>(&value)
}

fn require_true_value<T>(value: &Value) -> Result<(), OpenCodeAdapterError>
where
    T: DeserializeOwned,
{
    validate_generated::<T>(value)?;
    if value == &Value::Bool(true) {
        Ok(())
    } else {
        Err(OpenCodeDecodeError::AdmissionMismatch("boolean acknowledgement").into())
    }
}

fn validate_project_scope(
    value: &Value,
    expected_directory: Option<&str>,
) -> Result<(), OpenCodeAdapterError> {
    validate_generated::<crate::wire::ProjectCurrentResponse>(value)?;
    if expected_directory
        .is_some_and(|expected| value.get("worktree").and_then(Value::as_str) != Some(expected))
    {
        return Err(OpenCodeDecodeError::ScopeMismatch.into());
    }
    Ok(())
}

fn checked(response: Response) -> Result<Response, OpenCodeAdapterError> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(OpenCodeAdapterError::HttpStatus {
            status: response.status().as_u16(),
            body: "response body omitted".to_owned(),
        })
    }
}

fn path_segment(value: &str) -> Result<String, OpenCodeAdapterError> {
    if value.is_empty()
        || value
            .chars()
            .any(|character| character == '/' || character == '\\' || character.is_control())
    {
        return Err(OpenCodeAdapterError::Decode(
            OpenCodeDecodeError::ScopeMismatch,
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn receipt() -> crate::wire::SessionPromptV2Response {
        serde_json::from_value(json!({
            "data": {
                "admittedSeq": 7,
                "id": "msg_123",
                "sessionID": "ses_123",
                "prompt": {"text": "hello"},
                "delivery": "queue",
                "timeCreated": 12.5
            }
        }))
        .expect("generated v2 response fixture should decode")
    }

    #[test]
    fn durable_admission_rejects_session_id_and_delivery_mismatches() {
        let value = receipt();
        assert!(matches!(
            decode_prompt_admission(value.clone(), "ses_other", None, PromptDelivery::Queue),
            Err(OpenCodeAdapterError::Decode(
                OpenCodeDecodeError::ScopeMismatch
            ))
        ));
        assert!(matches!(
            decode_prompt_admission(
                value.clone(),
                "ses_123",
                Some("msg_other"),
                PromptDelivery::Queue
            ),
            Err(OpenCodeAdapterError::Decode(
                OpenCodeDecodeError::AdmissionMismatch("message id")
            ))
        ));
        assert!(matches!(
            decode_prompt_admission(value, "ses_123", None, PromptDelivery::Steer),
            Err(OpenCodeAdapterError::Decode(
                OpenCodeDecodeError::AdmissionMismatch("delivery")
            ))
        ));
    }

    #[test]
    fn durable_admission_maps_generated_receipt() {
        let admission =
            decode_prompt_admission(receipt(), "ses_123", Some("msg_123"), PromptDelivery::Queue)
                .expect("receipt should satisfy request scope");
        assert_eq!(admission.admitted_seq, 7);
        assert_eq!(admission.id, "msg_123");
        assert_eq!(admission.session_id, "ses_123");
        assert_eq!(admission.prompt, "hello");
        assert_eq!(admission.delivery, PromptDelivery::Queue);
    }

    #[test]
    fn generated_rest_requests_reject_out_of_contract_fields() {
        assert!(generated_body::<crate::wire::CreateSessionRequest>(json!({
            "title": "valid"
        }))
        .is_ok());
        assert!(generated_body::<crate::wire::CreateSessionRequest>(json!({
            "invented": true
        }))
        .is_err());
        assert!(generated_body::<crate::wire::PromptRequest>(json!({
            "parts": [{"type": "text", "text": "hello"}]
        }))
        .is_ok());
        assert!(generated_body::<crate::wire::PromptRequest>(json!({
            "parts": [{"type": "text"}]
        }))
        .is_err());
    }

    #[test]
    fn abort_requires_a_generated_true_acknowledgement() {
        assert!(require_true_value::<crate::wire::AbortSessionResponse>(&json!(true)).is_ok());
        assert!(require_true_value::<crate::wire::AbortSessionResponse>(&json!(false)).is_err());
        assert!(require_true_value::<crate::wire::AbortSessionResponse>(&json!({})).is_err());
    }

    #[test]
    fn generated_project_response_must_match_the_scoped_directory() {
        let project = json!({
            "id": "project",
            "worktree": "C:/scoped",
            "time": {"created": 1, "updated": 2},
            "sandboxes": []
        });
        assert!(validate_project_scope(&project, Some("C:/scoped")).is_ok());
        assert!(matches!(
            validate_project_scope(&project, Some("C:/other")),
            Err(OpenCodeAdapterError::Decode(
                OpenCodeDecodeError::ScopeMismatch
            ))
        ));
    }
}

fn validate_session_value(value: Value) -> Result<Value, OpenCodeAdapterError> {
    let object = value
        .as_object()
        .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
    if !id.starts_with("ses") {
        return Err(OpenCodeDecodeError::ScopeMismatch.into());
    }
    serde_json::from_value::<crate::wire::Session>(value.clone())
        .map_err(OpenCodeDecodeError::MalformedJson)?;
    Ok(value)
}

fn validate_message_value(value: Value) -> Result<Value, OpenCodeAdapterError> {
    let object = value
        .as_object()
        .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
    let info = object
        .get("info")
        .and_then(Value::as_object)
        .ok_or(OpenCodeDecodeError::UnsupportedShape)?;
    if !info
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.starts_with("msg"))
    {
        return Err(OpenCodeDecodeError::ScopeMismatch.into());
    }
    let info_value = Value::Object(info.clone());
    serde_json::from_value::<crate::wire::Message>(info_value)
        .map_err(OpenCodeDecodeError::MalformedJson)?;
    Ok(value)
}

/// One parsed SSE event.  `cursor` is intentionally optional: OpenCode's
/// `/event` currently emits no event id, so a reconnect cannot claim lossless
/// replay and must rebuild from REST first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: Vec<u8>,
    pub cursor: Option<String>,
}

#[derive(Debug)]
pub struct SseStream {
    reader: BufReader<Response>,
    pending_event: Option<String>,
    pending_data: Vec<String>,
    pending_id: Option<String>,
}

impl SseStream {
    pub fn next_event(&mut self) -> Result<Option<SseEvent>, OpenCodeAdapterError> {
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|error| OpenCodeAdapterError::Transport(error.to_string()))?;
            if read == 0 {
                if self.pending_event.is_some() || !self.pending_data.is_empty() {
                    return self.finish_event().map(Some);
                }
                return Ok(None);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                if self.pending_event.is_some() || !self.pending_data.is_empty() {
                    return self.finish_event().map(Some);
                }
                continue;
            }
            if let Some(value) = trimmed.strip_prefix("event:") {
                self.pending_event = Some(value.trim().to_owned());
            } else if let Some(value) = trimmed.strip_prefix("data:") {
                self.pending_data.push(value.trim_start().to_owned());
            } else if let Some(value) = trimmed.strip_prefix("id:") {
                self.pending_id = Some(value.trim().to_owned());
            } else if trimmed.starts_with(':') {
                continue;
            } else {
                return Err(OpenCodeDecodeError::UnsupportedShape.into());
            }
        }
    }

    fn finish_event(&mut self) -> Result<SseEvent, OpenCodeAdapterError> {
        let data = self.pending_data.join("\n").into_bytes();
        if data.is_empty() {
            return Err(OpenCodeDecodeError::UnsupportedShape.into());
        }
        let event = SseEvent {
            event_type: self.pending_event.take(),
            data,
            cursor: self.pending_id.take(),
        };
        self.pending_data.clear();
        Ok(event)
    }
}
