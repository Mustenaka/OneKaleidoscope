//! The pinned decode surface (ADR-0012 D-2).
//!
//! Every value this adapter reads from upstream traffic is declared here, once,
//! with four facts: what canonical purpose it serves, which upstream methods
//! carry it, the JSON Pointer that locates it in a live payload, and where the
//! same field is declared in the committed schema snapshot. Paths outside this
//! table are unreadable — the decoder has no other way to reach a value.
//!
//! ADR-0012 chose this over a generated client because the generators on hand
//! could not digest the Codex schema, and hand-written upstream types would be
//! a shadow protocol nobody checks. A pinned path is checkable: the drift test
//! resolves every schema pointer against the snapshot and every surface
//! identifier against `schemas/required-surface.toml`, so a silently reshaped
//! upstream fails a test rather than a phone.
//!
//! Only what this slice actually decodes is registered. Item kinds this
//! repository has no recording of — a shell command execution, for instance —
//! are deliberately absent, so they reduce to an
//! `UnknownUpstreamLabel` diagnostic instead of a guess.

/// Whether a missing value is normal or a protocol violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// The value must be present. Absence is
    /// [`crate::error::CodexAdapterError::PointerUnresolved`], never a silent
    /// default (ADR-0012 D-3).
    Required,
    /// The field is genuinely optional upstream.
    Optional,
}

/// Where a pinned field is declared in the committed schema snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaAnchor {
    /// File name under `schemas/codex`.
    pub document: &'static str,
    /// Pointer that must resolve inside that document.
    pub pointer: &'static str,
    /// Title the enclosing definition must carry, when the anchor addresses a
    /// positional `oneOf` branch. Without it, an upstream reordering of the
    /// branches would still resolve and silently mean something else.
    pub title: Option<&'static str>,
}

/// One canonical use for one upstream value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SurfacePurpose {
    SessionThreadId,
    SessionProjectRoot,
    SessionStartedThreadId,
    TurnResponseTurnId,
    TurnResponseStatus,
    TurnNotificationThreadId,
    TurnNotificationTurnId,
    TurnNotificationStatus,
    TurnNotificationItemsView,
    TurnNotificationStartedAt,
    TurnNotificationCompletedAt,
    TurnNotificationError,
    ThreadStatusThreadId,
    ThreadStatusType,
    ItemLifecycleThreadId,
    ItemLifecycleTurnId,
    ItemStartedAt,
    ItemCompletedAt,
    ItemIdentifier,
    ItemType,
    UserMessageContent,
    UserMessageContentText,
    AgentMessageText,
    AgentMessagePhase,
    ReasoningSummary,
    ReasoningContent,
    FileChangeStatus,
    FileChangeEntries,
    FileChangeEntryPath,
    FileChangeEntryKind,
    FileChangeEntryDiff,
    DeltaThreadId,
    DeltaTurnId,
    DeltaItemId,
    DeltaText,
    ApprovalThreadId,
    ApprovalTurnId,
    ApprovalItemId,
    ApprovalStartedAt,
    ApprovalReason,
    ApprovalDecision,
}

/// A single declared path.
#[derive(Debug, Clone, Copy)]
pub struct PinnedPath {
    pub purpose: SurfacePurpose,
    /// What canonical state this value ends up in.
    pub canonical_use: &'static str,
    /// Upstream methods whose payloads carry this value.
    pub methods: &'static [&'static str],
    /// JSON Pointer into the payload. Pointers marked
    /// [`Scope::Element`] are resolved against one array element rather than
    /// the whole payload.
    pub pointer: &'static str,
    pub scope: Scope,
    pub requirement: Requirement,
    pub anchors: &'static [SchemaAnchor],
    /// Identifiers in `schemas/required-surface.toml` that own this field.
    pub surface_entry_ids: &'static [&'static str],
}

/// What a pointer is resolved against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The whole upstream frame.
    Payload,
    /// One element of a collection already located by another pinned path.
    Element,
}

const NOTIFICATIONS: &str = "ServerNotification.json";
const PROTOCOL_V2: &str = "codex_app_server_protocol.v2.schemas.json";
const FILE_CHANGE_APPROVAL_PARAMS: &str = "FileChangeRequestApprovalParams.json";
const FILE_CHANGE_APPROVAL_RESPONSE: &str = "FileChangeRequestApprovalResponse.json";

/// The approval family this slice models.
///
/// Codex sends three approval requests, and it is tempting to treat them as one
/// because their parameters do agree. Their *replies* do not: a file-change
/// approval answers with a `decision` string, a command-execution approval
/// answers with a union that also has object branches, and a permissions
/// approval answers with a granted permission profile and no decision at all.
///
/// Only the file-change family has a recorded reply in this repository, so only
/// it is modelled. Presenting the other two as approvals would give a reader an
/// allow/deny pair the runtime never offered, which section 4.7 forbids; they
/// reduce to an unmodelled-message diagnostic until a recording exists.
pub const APPROVAL_METHODS: &[&str] = &["item/fileChange/requestApproval"];

const ITEM_LIFECYCLE_METHODS: &[&str] = &["item/started", "item/completed"];
const TURN_NOTIFICATION_METHODS: &[&str] = &["turn/started", "turn/completed"];
const DELTA_METHODS: &[&str] = &["item/agentMessage/delta"];

const APPROVAL_PARAM_ENTRY_IDS: &[&str] = &[
    "codex.type.FileChangeRequestApprovalParams",
    "codex.method.file-change-request-approval",
];

/// Every path this adapter is allowed to read.
pub const PINNED_PATHS: &[PinnedPath] = &[
    PinnedPath {
        purpose: SurfacePurpose::SessionThreadId,
        canonical_use: "provider-private thread identifier behind the session binding handle",
        methods: &["thread/start"],
        pointer: "/result/thread/id",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[
            SchemaAnchor {
                document: PROTOCOL_V2,
                pointer: "/definitions/ThreadStartResponse/properties/thread",
                title: None,
            },
            SchemaAnchor {
                document: PROTOCOL_V2,
                pointer: "/definitions/Thread/properties/id",
                title: None,
            },
        ],
        surface_entry_ids: &[
            "codex.type.ThreadStartResponse",
            "codex.method.thread-start",
        ],
    },
    PinnedPath {
        purpose: SurfacePurpose::SessionProjectRoot,
        canonical_use: "ProjectBinding.root_ref",
        methods: &["thread/start"],
        pointer: "/result/cwd",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: PROTOCOL_V2,
            pointer: "/definitions/ThreadStartResponse/properties/cwd",
            title: None,
        }],
        surface_entry_ids: &["codex.type.ThreadStartResponse"],
    },
    PinnedPath {
        purpose: SurfacePurpose::SessionStartedThreadId,
        canonical_use: "idempotent confirmation of SessionUpserted",
        methods: &["thread/started"],
        pointer: "/params/thread/id",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/ThreadStartedNotification/properties/thread",
                title: None,
            },
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/Thread/properties/id",
                title: None,
            },
        ],
        surface_entry_ids: &[
            "codex.method.thread-started",
            "codex.type.ThreadStartedNotification",
        ],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnResponseTurnId,
        canonical_use: "provider-private turn identifier behind the turn binding handle",
        methods: &["turn/start"],
        pointer: "/result/turn/id",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[
            SchemaAnchor {
                document: PROTOCOL_V2,
                pointer: "/definitions/TurnStartResponse/properties/turn",
                title: None,
            },
            SchemaAnchor {
                document: PROTOCOL_V2,
                pointer: "/definitions/Turn/properties/id",
                title: None,
            },
        ],
        surface_entry_ids: &["codex.type.TurnStartResponse", "codex.method.turn-start"],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnResponseStatus,
        canonical_use: "Turn.status on creation",
        methods: &["turn/start"],
        pointer: "/result/turn/status",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: PROTOCOL_V2,
            pointer: "/definitions/Turn/properties/status",
            title: None,
        }],
        surface_entry_ids: &["codex.type.TurnStartResponse"],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnNotificationThreadId,
        canonical_use: "session scope of a turn transition",
        methods: TURN_NOTIFICATION_METHODS,
        pointer: "/params/threadId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/TurnStartedNotification/properties/threadId",
                title: None,
            },
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/TurnCompletedNotification/properties/threadId",
                title: None,
            },
        ],
        surface_entry_ids: &["codex.method.turn-started", "codex.method.turn-completed"],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnNotificationTurnId,
        canonical_use: "Turn identity of a turn transition",
        methods: TURN_NOTIFICATION_METHODS,
        pointer: "/params/turn/id",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/Turn/properties/id",
            title: None,
        }],
        surface_entry_ids: &[
            "codex.type.TurnStartedNotification",
            "codex.type.TurnCompletedNotification",
        ],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnNotificationStatus,
        canonical_use: "Turn.status; the only source of a turn outcome",
        methods: TURN_NOTIFICATION_METHODS,
        pointer: "/params/turn/status",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/Turn/properties/status",
            title: None,
        }],
        surface_entry_ids: &["codex.type.TurnCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnNotificationItemsView,
        canonical_use: "proof that a completion payload is a partial view, never a transcript",
        methods: TURN_NOTIFICATION_METHODS,
        pointer: "/params/turn/itemsView",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/Turn/properties/itemsView",
            title: None,
        }],
        surface_entry_ids: &["codex.type.TurnCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnNotificationStartedAt,
        canonical_use: "Turn.started_at_ms",
        methods: TURN_NOTIFICATION_METHODS,
        pointer: "/params/turn/startedAt",
        scope: Scope::Payload,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/Turn/properties/startedAt",
            title: None,
        }],
        surface_entry_ids: &["codex.type.TurnStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnNotificationCompletedAt,
        canonical_use: "Turn.completed_at_ms",
        methods: TURN_NOTIFICATION_METHODS,
        pointer: "/params/turn/completedAt",
        scope: Scope::Payload,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/Turn/properties/completedAt",
            title: None,
        }],
        surface_entry_ids: &["codex.type.TurnCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::TurnNotificationError,
        canonical_use: "Turn.error, populated only for a failed turn",
        methods: TURN_NOTIFICATION_METHODS,
        pointer: "/params/turn/error",
        scope: Scope::Payload,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/Turn/properties/error",
            title: None,
        }],
        surface_entry_ids: &["codex.type.TurnCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ThreadStatusThreadId,
        canonical_use: "session scope of a runtime status report",
        methods: &["thread/status/changed"],
        pointer: "/params/threadId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadStatusChangedNotification/properties/threadId",
            title: None,
        }],
        surface_entry_ids: &["codex.method.thread-status-changed"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ThreadStatusType,
        canonical_use: "runtime-reported session status",
        methods: &["thread/status/changed"],
        pointer: "/params/status/type",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadStatus/oneOf/1/properties/type",
            title: Some("IdleThreadStatus"),
        }],
        surface_entry_ids: &["codex.type.ThreadStatusChangedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ItemLifecycleThreadId,
        canonical_use: "session scope of an item transition",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/threadId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/ItemStartedNotification/properties/threadId",
                title: None,
            },
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/ItemCompletedNotification/properties/threadId",
                title: None,
            },
        ],
        surface_entry_ids: &["codex.method.item-started", "codex.method.item-completed"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ItemLifecycleTurnId,
        canonical_use: "Item.turn_id",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/turnId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/ItemStartedNotification/properties/turnId",
                title: None,
            },
            SchemaAnchor {
                document: NOTIFICATIONS,
                pointer: "/definitions/ItemCompletedNotification/properties/turnId",
                title: None,
            },
        ],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ItemStartedAt,
        canonical_use: "Item.created_at_ms",
        methods: &["item/started"],
        pointer: "/params/startedAtMs",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ItemStartedNotification/properties/startedAtMs",
            title: None,
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ItemCompletedAt,
        canonical_use: "Item.updated_at_ms",
        methods: &["item/completed"],
        pointer: "/params/completedAtMs",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ItemCompletedNotification/properties/completedAtMs",
            title: None,
        }],
        surface_entry_ids: &["codex.type.ItemCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ItemIdentifier,
        canonical_use: "provider-private item identifier behind the item binding handle",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/id",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/0/properties/id",
            title: Some("UserMessageThreadItem"),
        }],
        surface_entry_ids: &["codex.method.item-started", "codex.method.item-completed"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ItemType,
        canonical_use: "selects the ItemBody variant; unknown values become a diagnostic",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/type",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/0/properties/type",
            title: Some("UserMessageThreadItem"),
        }],
        surface_entry_ids: &["codex.method.item-started"],
    },
    PinnedPath {
        purpose: SurfacePurpose::UserMessageContent,
        canonical_use: "ItemBody::UserMessage content parts",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/content",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/0/properties/content",
            title: Some("UserMessageThreadItem"),
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::UserMessageContentText,
        canonical_use: "text of one user message part",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/text",
        scope: Scope::Element,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/UserInput/oneOf/0/properties/text",
            title: Some("TextUserInput"),
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::AgentMessageText,
        canonical_use: "ItemBody::AgentMessage content",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/text",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/2/properties/text",
            title: Some("AgentMessageThreadItem"),
        }],
        surface_entry_ids: &["codex.type.ItemCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::AgentMessagePhase,
        canonical_use: "MessagePhase; absent upstream means the phase is unknown",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/phase",
        scope: Scope::Payload,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/2/properties/phase",
            title: Some("AgentMessageThreadItem"),
        }],
        surface_entry_ids: &["codex.type.ItemCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ReasoningSummary,
        canonical_use: "ItemBody::Reasoning summary lines",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/summary",
        scope: Scope::Payload,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/4/properties/summary",
            title: Some("ReasoningThreadItem"),
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ReasoningContent,
        canonical_use: "ItemBody::Reasoning content lines",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/content",
        scope: Scope::Payload,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/4/properties/content",
            title: Some("ReasoningThreadItem"),
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::FileChangeStatus,
        canonical_use: "ItemStatus of a file edit, including the declined terminal state",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/status",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/6/properties/status",
            title: Some("FileChangeThreadItem"),
        }],
        surface_entry_ids: &["codex.type.ItemCompletedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::FileChangeEntries,
        canonical_use: "ChangeSet entries of ItemBody::FileEdit",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/params/item/changes",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/ThreadItem/oneOf/6/properties/changes",
            title: Some("FileChangeThreadItem"),
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::FileChangeEntryPath,
        canonical_use: "FileChange.path_ref; a full path, therefore sensitive",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/path",
        scope: Scope::Element,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/FileUpdateChange/properties/path",
            title: None,
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::FileChangeEntryKind,
        canonical_use: "FileChangeKind",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/kind/type",
        scope: Scope::Element,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/PatchChangeKind/oneOf/0/properties/type",
            title: Some("AddPatchChangeKind"),
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::FileChangeEntryDiff,
        canonical_use: "FileChange.diff; sensitive payload behind a reference",
        methods: ITEM_LIFECYCLE_METHODS,
        pointer: "/diff",
        scope: Scope::Element,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/FileUpdateChange/properties/diff",
            title: None,
        }],
        surface_entry_ids: &["codex.type.ItemStartedNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::DeltaThreadId,
        canonical_use: "session scope of a streaming update",
        methods: DELTA_METHODS,
        pointer: "/params/threadId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/AgentMessageDeltaNotification/properties/threadId",
            title: None,
        }],
        surface_entry_ids: &["codex.method.item-agent-message-delta"],
    },
    PinnedPath {
        purpose: SurfacePurpose::DeltaTurnId,
        canonical_use: "turn scope of a streaming update",
        methods: DELTA_METHODS,
        pointer: "/params/turnId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/AgentMessageDeltaNotification/properties/turnId",
            title: None,
        }],
        surface_entry_ids: &["codex.type.AgentMessageDeltaNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::DeltaItemId,
        canonical_use: "the existing item a delta extends; never a new item",
        methods: DELTA_METHODS,
        pointer: "/params/itemId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/AgentMessageDeltaNotification/properties/itemId",
            title: None,
        }],
        surface_entry_ids: &["codex.type.AgentMessageDeltaNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::DeltaText,
        canonical_use: "text appended to an existing agent message",
        methods: DELTA_METHODS,
        pointer: "/params/delta",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: NOTIFICATIONS,
            pointer: "/definitions/AgentMessageDeltaNotification/properties/delta",
            title: None,
        }],
        surface_entry_ids: &["codex.type.AgentMessageDeltaNotification"],
    },
    PinnedPath {
        purpose: SurfacePurpose::ApprovalThreadId,
        canonical_use: "session scope of an approval request",
        methods: APPROVAL_METHODS,
        pointer: "/params/threadId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: FILE_CHANGE_APPROVAL_PARAMS,
            pointer: "/properties/threadId",
            title: None,
        }],
        surface_entry_ids: APPROVAL_PARAM_ENTRY_IDS,
    },
    PinnedPath {
        purpose: SurfacePurpose::ApprovalTurnId,
        canonical_use: "turn scope of an approval request",
        methods: APPROVAL_METHODS,
        pointer: "/params/turnId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: FILE_CHANGE_APPROVAL_PARAMS,
            pointer: "/properties/turnId",
            title: None,
        }],
        surface_entry_ids: APPROVAL_PARAM_ENTRY_IDS,
    },
    PinnedPath {
        purpose: SurfacePurpose::ApprovalItemId,
        canonical_use: "the only join key an approval request carries",
        methods: APPROVAL_METHODS,
        pointer: "/params/itemId",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: FILE_CHANGE_APPROVAL_PARAMS,
            pointer: "/properties/itemId",
            title: None,
        }],
        surface_entry_ids: APPROVAL_PARAM_ENTRY_IDS,
    },
    PinnedPath {
        purpose: SurfacePurpose::ApprovalStartedAt,
        canonical_use: "AttentionItem.created_at_ms",
        methods: APPROVAL_METHODS,
        pointer: "/params/startedAtMs",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: FILE_CHANGE_APPROVAL_PARAMS,
            pointer: "/properties/startedAtMs",
            title: None,
        }],
        surface_entry_ids: APPROVAL_PARAM_ENTRY_IDS,
    },
    PinnedPath {
        purpose: SurfacePurpose::ApprovalReason,
        canonical_use: "ApprovalRequest.detail_ref, when the runtime explains itself",
        methods: APPROVAL_METHODS,
        pointer: "/params/reason",
        scope: Scope::Payload,
        requirement: Requirement::Optional,
        anchors: &[SchemaAnchor {
            document: FILE_CHANGE_APPROVAL_PARAMS,
            pointer: "/properties/reason",
            title: None,
        }],
        surface_entry_ids: APPROVAL_PARAM_ENTRY_IDS,
    },
    PinnedPath {
        purpose: SurfacePurpose::ApprovalDecision,
        canonical_use: "the human decision echoed back to the runtime",
        methods: APPROVAL_METHODS,
        pointer: "/result/decision",
        scope: Scope::Payload,
        requirement: Requirement::Required,
        anchors: &[SchemaAnchor {
            document: FILE_CHANGE_APPROVAL_RESPONSE,
            pointer: "/properties/decision",
            title: None,
        }],
        surface_entry_ids: &["codex.type.FileChangeRequestApprovalResponse"],
    },
];

/// Looks up the one declaration for a purpose.
///
/// The decoder has no other way to obtain a pointer, which is what makes the
/// table exhaustive rather than advisory.
pub fn path(purpose: SurfacePurpose) -> Option<&'static PinnedPath> {
    PINNED_PATHS.iter().find(|entry| entry.purpose == purpose)
}

/// Every upstream method the table declares.
pub fn declared_methods() -> Vec<&'static str> {
    let mut methods = PINNED_PATHS
        .iter()
        .flat_map(|entry| entry.methods.iter().copied())
        .collect::<Vec<_>>();
    methods.sort_unstable();
    methods.dedup();
    methods
}

/// Whether a method has any pinned path at all.
pub fn method_is_declared(method: &str) -> bool {
    PINNED_PATHS
        .iter()
        .any(|entry| entry.methods.contains(&method))
}
