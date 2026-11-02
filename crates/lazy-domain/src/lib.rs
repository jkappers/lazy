//! Typed identities, message documents, and the pure rules shared by Lazy's server and clients.
//!
//! Nothing here performs I/O or depends on a UI toolkit, so the same code runs on the server,
//! on every desktop client, and on future phone clients.

pub mod attachment;
pub mod authz;
pub mod board;
pub mod conversation;
pub mod document;
pub mod emoji;
pub mod fuzzy;
pub mod ids;
pub mod markdown;
pub mod mention;
pub mod message;
pub mod names;
pub mod notification;
pub mod page;
pub mod search;
pub mod time_stamp;
pub mod workspace;

pub use authz::{
    ChannelAccess, ChannelVisibility, MAX_GROUP_MEMBERS, MAX_GROUPS_PER_WORKSPACE,
    MAX_WEBHOOK_BODY_BYTES, MAX_WEBHOOKS_PER_CONVERSATION, WorkspaceRole,
};
pub use board::{Board, BoardEdge, BoardError, BoardNode};
pub use conversation::{ConversationFacts, ConversationKind, ParticipantError, ParticipantSet};
pub use document::{Block, DocumentError, Inline, Marks, MessageDocument};
pub use emoji::{MAX_CUSTOM_EMOJI, MAX_REACTION_KINDS, ReactionEmoji};
pub use ids::{
    AccessTokenId, AttachmentId, ConversationId, EmojiId, EventSeq, GroupId, InvitationId,
    MessageId, NotationId, NotationReplyId, OperationId, PageId, UserId, WebhookId, WorkspaceId,
};
pub use mention::{MapDirectory, MentionDirectory, MentionTarget, NoMentions};
pub use message::{MAX_THREAD_REPLIERS_LISTED, MessageFacts, Permalink};
pub use names::{
    ChannelName, DisplayName, EmojiName, GroupHandle, NameError, Username, WorkspaceName,
};
pub use page::{PageAccess, PageContent, PageError, PageKind, VersionReason};
pub use search::{FileKind, SearchError, SearchQuery};
pub use time_stamp::Timestamp;
