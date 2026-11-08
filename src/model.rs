//! View-model types the UI renders. They are deliberately plain data so views
//! never reach into `aircoreclient` directly; the session layer produces them
//! from core state. [`mock`] supplies sample data for tests and for parts the
//! session doesn't load yet (messages).
//!
//! Identity always comes from IDs. Display names are peer-controlled and are
//! only ever shown, never compared.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Datelike as _, Local, Utc};
use gpui_kit::SharedString;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::markdown::{self, Markdown};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatId(pub SharedString);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserId(pub SharedString);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageId(pub SharedString);

/// A member of a group, as the details pane lists them.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// From the whole user id (uuid and domain): ids that share a uuid
    /// stay apart.
    pub id: UserId,
    /// One bounded line; "You" for the user.
    pub name: SharedString,
    pub own: bool,
    /// Being removed: the removal isn't committed yet.
    pub removing: bool,
    /// The room policy lets the user remove them (never in an ended group).
    pub can_remove: bool,
    /// Unique within the group, from a hash of the user id: tells apart
    /// members whose (peer-chosen) names are the same. `:kick #<tag>`.
    pub tag: SharedString,
    /// Their profile picture, if one could be drawn.
    pub picture: Option<Picture>,
}

/// One of the user's devices (a leaf of their self group).
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// One bounded line. The user's devices name each other, so names are
    /// often alike (two "Linux"): the tag tells them apart.
    pub name: SharedString,
    /// What the device runs on, as it said when linked ("" if unknown).
    pub platform: SharedString,
    pub linked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub this_device: bool,
    /// Unique among the devices, a prefix of the client id:
    /// `:device-name #<tag> …`, `:unlink #<tag>`.
    pub tag: SharedString,
}

/// The fewest leading characters, from `min` up, that tell `ids` apart
/// (all of them if nothing shorter does).
pub fn unique_prefix_len(ids: &[String], min: usize) -> usize {
    let longest = ids.iter().map(String::len).max().unwrap_or(0);
    let unique = |len: usize| {
        let prefixes: std::collections::HashSet<_> =
            ids.iter().map(|id| &id[..len.min(id.len())]).collect();
        prefixes.len() == ids.len()
    };
    (min..longest).find(|&len| unique(len)).unwrap_or(longest)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatSummary {
    pub id: ChatId,
    pub title: SharedString,
    pub is_group: bool,
    /// A contact request not accepted yet, or `None` for an established chat.
    pub request: Option<Request>,
    /// When an open request expires, if the core knows.
    pub request_expires: Option<DateTime<Utc>>,
    /// The chat with the user's own devices. Marked in the UI, so a peer
    /// can't pass off a chat titled "Notes to self" as it.
    pub is_self_chat: bool,
    /// A one-to-one chat (or contact request): what contact operations
    /// may do in it. `None` for groups and the self chat.
    pub one_to_one: Option<OneToOne>,
    pub preview: Option<Preview>,
    /// `None` for a chat without messages.
    pub last_activity: Option<DateTime<Utc>>,
    pub unread: u32,
    /// Alerts are off for it until then (this device only, as upstream).
    pub muted: Option<Mute>,
    /// A group the user is no longer in (left, removed, or it was
    /// deleted): only deleting it is left.
    pub group_ended: bool,
    /// The contact's profile picture or the group's, if one could be
    /// drawn (see `session::chats::picture_bytes`).
    pub picture: Option<Picture>,
}

impl ChatSummary {
    /// Alerts for it are off at `now`.
    pub fn is_muted(&self, now: DateTime<Utc>) -> bool {
        self.muted.is_some_and(|mute| mute.is_muted(now))
    }

    /// What deleting a message in it depends on.
    pub fn deletion_facts(&self) -> ChatFacts {
        ChatFacts {
            self_chat: self.is_self_chat,
            live: self.composes(),
        }
    }

    /// The user blocked the peer: their messages and requests are
    /// dropped, and the chat offers unblocking instead of a composer.
    pub fn blocked(&self) -> bool {
        self.one_to_one
            .is_some_and(|chat| chat.stage == Stage::Blocked)
    }

    /// Messages and files can be sent: see [`takes_messages`].
    pub fn composes(&self) -> bool {
        self.request.is_none() && takes_messages(self.one_to_one, self.group_ended)
    }

    /// A contact's chat that ended: they deleted it.
    pub fn ended(&self) -> bool {
        self.one_to_one
            == Some(OneToOne {
                stage: Stage::Ended,
                ..OneToOne::CONTACT
            })
    }
}

/// Whether a chat shows a picture, as upstream: a group's own (never the
/// self chat's), or an established contact's, not a requester's (they
/// aren't accepted yet) nor a blocked one's.
pub fn shows_picture(one_to_one: Option<OneToOne>, self_chat: bool) -> bool {
    match one_to_one {
        None => !self_chat,
        Some(chat) => chat.side == Side::Contact && chat.stage != Stage::Blocked,
    }
}

/// Messages and files can be sent to a chat: no request, not blocked,
/// not a contact's chat that ended (they deleted it), not a group the
/// user is out of. The one rule, from facts the view's summary and the
/// session's core state both have.
pub fn takes_messages(one_to_one: Option<OneToOne>, group_ended: bool) -> bool {
    !group_ended
        && one_to_one.is_none_or(|chat| {
            chat.side == Side::Contact && matches!(chat.stage, Stage::Open | Stage::Sending)
        })
}

/// A one-to-one chat as contact operations see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneToOne {
    pub side: Side,
    pub stage: Stage,
    /// Who the peer is is known; a request sent to a username learns it
    /// only once they answer.
    pub peer_known: bool,
}

/// Whose the chat is: a contact's, or a request either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Contact,
    Incoming,
    Outgoing,
}

/// The core's chat status: blocking is the peer's, and hides the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// An outgoing request not sent yet.
    Sending,
    Open,
    /// A request that expired or was withdrawn, or a deleted contact.
    Ended,
    Blocked,
}

impl OneToOne {
    pub const CONTACT: Self = Self {
        side: Side::Contact,
        stage: Stage::Open,
        peer_known: true,
    };

    /// The request the chat shows, if it isn't a contact's. Neither side
    /// can act on an ended or blocked one.
    pub fn request(self) -> Option<Request> {
        Some(match (self.side, self.stage) {
            (Side::Contact, _) => return None,
            (_, Stage::Ended | Stage::Blocked) => Request::Closed,
            (Side::Incoming, _) => Request::Incoming,
            (Side::Outgoing, _) => Request::Outgoing,
        })
    }
}

/// What the user can do to a contact (or request) and its chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactOp {
    Block,
    Unblock,
    Report,
    Retract,
    Delete,
}

impl ContactOp {
    /// Whether `self` applies to `chat`, or why not. The one rule table:
    /// the view asks before confirming, the session again on the core's
    /// state before running.
    ///
    /// Blocking an incoming request declines it (as upstream); an open
    /// outgoing request is retracted, not blocked or deleted, and an open
    /// incoming one declined, not deleted. Blocking hides whether a
    /// request was open: a blocked chat can always be unblocked or
    /// deleted.
    pub fn check(self, chat: OneToOne) -> Result<(), &'static str> {
        let blocked = chat.stage == Stage::Blocked;
        let open = matches!(chat.stage, Stage::Open | Stage::Sending);
        match self {
            ContactOp::Block if blocked => Err("They're blocked already"),
            ContactOp::Block if chat.side == Side::Outgoing => {
                Err("Retract your request instead: :retract")
            }
            ContactOp::Unblock if !blocked => Err("They aren't blocked"),
            ContactOp::Block | ContactOp::Unblock | ContactOp::Report if !chat.peer_known => {
                Err("Only their username is known until they answer")
            }
            ContactOp::Block | ContactOp::Unblock | ContactOp::Report => Ok(()),
            ContactOp::Retract if chat.side != Side::Outgoing => {
                Err("Only a contact request you sent can be retracted")
            }
            ContactOp::Retract if chat.stage == Stage::Sending => {
                Err("The request is still being sent; try again in a moment")
            }
            ContactOp::Retract if blocked => Err("You blocked them: delete the request instead"),
            ContactOp::Retract if !open => Err("That request has ended already"),
            ContactOp::Retract => Ok(()),
            ContactOp::Delete if open && chat.side == Side::Outgoing => {
                Err("Retract the request instead: :retract")
            }
            ContactOp::Delete if open && chat.side == Side::Incoming => {
                Err("Decline the request instead: :decline")
            }
            ContactOp::Delete => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Someone asked to connect; the user can accept or decline.
    Incoming,
    /// The user asked; waiting for the other side.
    Outgoing,
    /// Expired or withdrawn without a connection; only a record now.
    Closed,
}

/// The line under a chat's title: its draft or its last message.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub prefix: Option<Prefix>,
    pub text: SharedString,
    pub kind: PreviewKind,
}

/// Who the preview line is from. The client's own labels are kept apart
/// from peer names, so a peer called "You" can't pass for the local user.
#[derive(Debug, Clone, PartialEq)]
pub enum Prefix {
    Draft,
    You,
    /// A group member, by display name.
    Sender(SharedString),
}

impl Prefix {
    pub fn label(&self) -> SharedString {
        match self {
            Prefix::Draft => "Draft".into(),
            Prefix::You => "You".into(),
            Prefix::Sender(name) => name.clone(),
        }
    }

    /// Generated by the client rather than chosen by a peer.
    pub fn is_label(&self) -> bool {
        !matches!(self, Prefix::Sender(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Message,
    Draft,
    /// Stands in for a message that can't be shown (deleted, hidden).
    Notice,
}

/// Longest preview kept, in graphemes; a row shows one line of it anyway.
const MAX_PREVIEW: usize = 160;
/// Longest name or title kept, in graphemes.
const MAX_NAME: usize = 64;

/// A peer-controlled message preview as one line: see [`one_line`].
pub fn preview_line(text: &str) -> SharedString {
    one_line(text, MAX_PREVIEW)
}

/// A peer-controlled name or chat title as one line: see [`one_line`].
pub fn name_line(text: &str) -> SharedString {
    let name = one_line(text, MAX_NAME);
    // An all-invisible name would leave a blank row or sender prefix.
    if name.is_empty() {
        "Unnamed".into()
    } else {
        name
    }
}

/// Invisible and control characters dropped (no bidi tricks, no
/// look-alikes built from zero-width characters), whitespace runs
/// collapsed, and at most `max` graphemes, ending in "…" when cut.
fn one_line(text: &str, max: usize) -> SharedString {
    let visible: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !is_invisible(*c))
        .collect();
    let collapsed = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut graphemes = collapsed.graphemes(true);
    // A grapheme count alone doesn't bound the size: one cluster can stack
    // thousands of combining marks ("zalgo").
    let mut line: String = graphemes
        .by_ref()
        .take(max)
        .flat_map(|grapheme| grapheme.chars().take(MAX_GRAPHEME_CHARS))
        .collect();
    if graphemes.next().is_some() {
        line.push('…');
    }
    line.into()
}

/// Chars kept per grapheme: room for ZWJ emoji sequences (a family is
/// seven), not for stacks of combining marks.
const MAX_GRAPHEME_CHARS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: MessageId,
    /// For events, the user who caused them, if known.
    pub sender_id: Option<UserId>,
    /// Display name at render time; never used for identity.
    pub sender_name: SharedString,
    /// Set by the producer by comparing `sender_id` with the local user's ID.
    pub own: bool,
    pub body: MessageBody,
    pub sent_at: DateTime<Utc>,
    pub status: MessageStatus,
    pub edited: bool,
    /// The message this one replies to.
    pub reply_to: Option<Quote>,
    /// The replied-to message, when it is here and in this chat.
    pub reply_target: Option<MessageId>,
    /// See [`reactions`]: bounded, the user's own always among them.
    pub reactions: Vec<Reaction>,
    /// Kinds of reaction not shown.
    pub more_reactions: usize,
    /// Deleted for everyone: only a placeholder is left.
    pub deleted: bool,
    /// The core lets the user delete it on their devices (some system
    /// messages stay).
    pub erasable: bool,
}

/// One kind of reaction on a message, as shown under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reaction {
    /// The emoji as one visible grapheme (see [`reaction_label`]).
    pub label: SharedString,
    pub count: usize,
    /// The user reacted with it.
    pub own: bool,
}

/// The quick reactions the picker offers, in the order of its keys (1–6).
/// As upstream's reaction bar.
pub const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

/// Kinds of reaction shown under a message; peers decide how many there are.
const MAX_REACTIONS: usize = 12;

/// A peer's reaction emoji as shown: one visible grapheme ("…" after it if
/// they sent more), "?" if nothing visible is left. Reactions are any
/// text the peer chose; the core doesn't check them.
pub fn reaction_label(emoji: &str) -> SharedString {
    let label = one_line(emoji, 1);
    if label.is_empty() { "?".into() } else { label }
}

/// Whether the user gave `emoji` (as it is labelled) among `reactions`:
/// what a picker key retracts rather than adds.
pub fn gave(reactions: &[Reaction], emoji: &str) -> bool {
    let label = reaction_label(emoji);
    reactions
        .iter()
        .any(|reaction| reaction.own && reaction.label == label)
}

/// What a message's reactions show, from `(emoji, reactors)` in order of
/// first appearance; `me` is the user. Emoji that look the same once
/// labelled are counted together, each reactor once (a peer could react
/// with many invisible variants of one emoji). At most [`MAX_REACTIONS`]
/// kinds, but always the user's own (so they can see what they would
/// retract); returns how many kinds were left out.
pub fn reactions<'a, U: Eq + std::hash::Hash + 'a>(
    raw: impl IntoIterator<Item = (&'a str, &'a [U])>,
    me: &U,
) -> (Vec<Reaction>, usize) {
    let mut index: HashMap<SharedString, usize> = HashMap::new();
    let mut merged: Vec<(SharedString, HashSet<&'a U>)> = Vec::new();
    for (emoji, users) in raw {
        let label = reaction_label(emoji);
        let ix = *index.entry(label.clone()).or_insert_with(|| {
            merged.push((label, HashSet::new()));
            merged.len() - 1
        });
        merged[ix].1.extend(users);
    }
    let merged: Vec<Reaction> = merged
        .into_iter()
        .map(|(label, users)| Reaction {
            label,
            count: users.len(),
            own: users.contains(me),
        })
        .collect();
    let total = merged.len();
    let mut shown = Vec::new();
    for (ix, reaction) in merged.into_iter().enumerate() {
        if ix < MAX_REACTIONS || reaction.own {
            shown.push(reaction);
        }
    }
    let more = total - shown.len();
    (shown, more)
}

/// A replied-to message as shown above a reply: who wrote it and one
/// bounded line of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub sender: SharedString,
    pub text: SharedString,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageBody {
    Text {
        /// The markdown as sent; what copying a message copies.
        source: SharedString,
        markdown: Arc<Markdown>,
    },
    /// A system message (member added, title changed…), already in words.
    Event(SharedString),
    /// Stands in for content that can't be shown (deleted, hidden…).
    Notice(SharedString),
    /// A file or image. Its name and description are peer-chosen.
    Attachment(Attachment),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttachmentId(pub uuid::Uuid);

#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub id: AttachmentId,
    /// The sender's file name as one bounded line ("file" if blank).
    pub name: SharedString,
    pub description: Option<SharedString>,
    /// As the sender declared it.
    pub size: u64,
    pub state: AttachmentState,
    /// Only ever a preview the core made (or checked) and we decoded under
    /// size limits; never the received file itself, undecoded.
    pub image: Option<Picture>,
    /// The image's frames, when it moves and fits the limits. Drawn in
    /// place of `image`, which stays what it freezes on.
    pub animation: Option<Animation>,
}

/// An animated image, decoded under limits.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub picture: Picture,
    /// How long one loop takes.
    pub loop_time: std::time::Duration,
}

/// Times an animation plays by itself when limited. As upstream.
const MAX_AUTO_LOOPS: u32 = 3;

/// How long an animation plays by itself when limited, however long its
/// loops: gpui redraws the conversation at display rate while one is drawn,
/// whatever its frame delays, and a peer chooses those.
const MAX_AUTO_PLAY: std::time::Duration = std::time::Duration::from_secs(30);

impl Animation {
    /// Whether it still plays, `played` after it started: past its loops or
    /// [`MAX_AUTO_PLAY`] it freezes, when `limited`.
    pub fn plays(&self, played: std::time::Duration, limited: bool) -> bool {
        !limited || played < MAX_AUTO_PLAY.min(self.loop_time.saturating_mul(MAX_AUTO_LOOPS))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentState {
    Uploading,
    UploadFailed,
    Pending,
    Downloading,
    Ready,
    DownloadFailed,
    /// The server no longer has it.
    Gone,
}

impl AttachmentState {
    /// The file's bytes are on this device.
    pub fn is_local(self) -> bool {
        matches!(self, Self::Ready | Self::Uploading | Self::UploadFailed)
    }
}

/// A decoded image, ready for the GPU.
#[derive(Clone)]
pub struct Picture(pub Arc<gpui_kit::RenderImage>);

impl Picture {
    pub fn size(&self) -> (u32, u32) {
        let size = self.0.size(0);
        (size.width.0 as u32, size.height.0 as u32)
    }
}

impl PartialEq for Picture {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (width, height) = self.size();
        write!(f, "Picture({width}x{height})")
    }
}

/// The box an image preview is drawn in, in pixels.
pub const PREVIEW_BOX: (u32, u32) = (360, 270);

/// An image's size on screen: fit into `max` keeping its aspect ratio,
/// never scaled up, never collapsing to nothing.
pub fn fit(size: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (width, height) = (size.0.max(1) as f64, size.1.max(1) as f64);
    let scale = (max.0 as f64 / width).min(max.1 as f64 / height).min(1.0);
    let edge = |e: f64| ((e * scale).round() as u32).max(1);
    (edge(width), edge(height))
}

/// "3 B", "1.5 KB", "12 MB": decimal units, one decimal below ten.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 9.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Longest file name saved, in bytes (most file systems allow 255).
const MAX_FILE_NAME: usize = 200;

/// A peer-chosen file name made safe to save under: one plain component
/// (no separators, no `..`, not hidden), no control or invisible
/// characters, bounded, keeping a short extension when cut.
pub fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .filter(|c| !is_invisible(*c))
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    // Leading dots would hide the file (or make it `..`); trailing dots
    // and spaces are dropped by some systems, changing the name.
    let cleaned = cleaned
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    if cleaned.is_empty() {
        return "attachment".into();
    }
    if cleaned.len() <= MAX_FILE_NAME {
        return cleaned.into();
    }
    let extension = cleaned
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| extension.len() <= 16)
        .unwrap_or("");
    let room = MAX_FILE_NAME - extension.len() - 1;
    let mut cut = room;
    while !cleaned.is_char_boundary(cut) {
        cut -= 1;
    }
    let stem = cleaned[..cut].trim_end_matches(['.', ' ']);
    if extension.is_empty() {
        stem.into()
    } else {
        format!("{stem}.{extension}")
    }
}

/// The `n`th alternative to a taken file name: "a.txt" → "a (2).txt".
pub fn numbered_file_name(name: &str, n: usize) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} ({n}).{extension}"),
        _ => format!("{name} ({n})"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    Sending,
    Sent,
    Delivered,
    Read,
    Failed,
}

impl Message {
    pub fn text(
        id: MessageId,
        sender: (UserId, SharedString, bool),
        source: &str,
        sent_at: DateTime<Utc>,
    ) -> Self {
        let (sender_id, sender_name, own) = sender;
        Self {
            id,
            sender_id: Some(sender_id),
            sender_name,
            own,
            body: MessageBody::Text {
                source: source.to_string().into(),
                markdown: Arc::new(markdown::parse(source)),
            },
            sent_at,
            status: MessageStatus::Sent,
            edited: false,
            reply_to: None,
            reply_target: None,
            reactions: Vec::new(),
            more_reactions: 0,
            deleted: false,
            erasable: true,
        }
    }

    /// The text copying the message gives.
    pub fn plain_text(&self) -> SharedString {
        match &self.body {
            MessageBody::Text { source, .. } => source.clone(),
            MessageBody::Event(text) | MessageBody::Notice(text) => text.clone(),
            MessageBody::Attachment(attachment) => attachment.name.clone(),
        }
    }

    pub fn is_event(&self) -> bool {
        matches!(self.body, MessageBody::Event(_))
    }

    /// The user's file that failed to upload: it can be sent again.
    pub fn upload_failed(&self) -> bool {
        self.own
            && matches!(&self.body, MessageBody::Attachment(attachment)
                if attachment.state == AttachmentState::UploadFailed)
    }

    /// What deleting it depends on.
    pub fn deletion_facts(&self) -> MessageFacts {
        MessageFacts {
            content: !self.is_event(),
            own: self.own,
            sent: !matches!(self.status, MessageStatus::Sending | MessageStatus::Failed),
            deleted: self.deleted,
            erasable: self.erasable,
        }
    }
}

/// Who a message is deleted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteFor {
    /// On all the user's devices; the other members keep it.
    Me,
    /// Replaced by a placeholder for every member.
    Everyone,
}

/// A message as deleting it sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageFacts {
    /// Sent by someone (not a system message).
    pub content: bool,
    pub own: bool,
    /// The server took it.
    pub sent: bool,
    /// Deleted for everyone already.
    pub deleted: bool,
    /// The core lets the user delete it on their devices.
    pub erasable: bool,
}

/// The chat a message is deleted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatFacts {
    pub self_chat: bool,
    /// Messages can be sent to it (a deletion for everyone is one).
    pub live: bool,
}

impl DeleteFor {
    /// Whether `self` applies to `message` in `chat`, or why not. The one
    /// rule table: the view asks before confirming, the session again on
    /// the core's state before running.
    ///
    /// As upstream: only the sender deletes a message for everyone, once
    /// it was sent, and not in Notes to self (deleting it there already
    /// removes it from every device). The core itself doesn't check who
    /// sent a message it is asked to delete for everyone.
    pub fn check(self, message: MessageFacts, chat: ChatFacts) -> Result<(), &'static str> {
        match self {
            DeleteFor::Me if message.erasable => Ok(()),
            DeleteFor::Me => Err("Air keeps that message"),
            DeleteFor::Everyone if !message.content => {
                Err("Only messages can be deleted for everyone")
            }
            DeleteFor::Everyone if !message.own => {
                Err("Only your own messages can be deleted for everyone; delete it for you instead")
            }
            DeleteFor::Everyone if message.deleted => Err("It's deleted for everyone already"),
            DeleteFor::Everyone if chat.self_chat => {
                Err("In Notes to self, deleting it for you removes it everywhere")
            }
            DeleteFor::Everyone if !message.sent => {
                Err("It wasn't sent; delete it for you instead")
            }
            DeleteFor::Everyone if !chat.live => {
                Err("This chat takes no messages; delete it for you instead")
            }
            DeleteFor::Everyone => Ok(()),
        }
    }
}

/// One version of an edited message.
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    /// `None` when it isn't text.
    pub markdown: Option<Arc<Markdown>>,
    /// When it was sent (the first) or the edit made, as the sender says.
    pub at: DateTime<Utc>,
}

/// Versions of a message the details show; a sender can edit often.
pub const MAX_VERSIONS: usize = 20;

/// A message's edit history as the details show it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditHistory {
    /// Newest first, at most [`MAX_VERSIONS`]; the last is the original
    /// when `older` is 0.
    pub versions: Vec<Version>,
    /// Older versions left out.
    pub older: usize,
}

impl EditHistory {
    /// From the core's versions, oldest first, after the oldest
    /// `skipped` (the caller may leave out what won't be shown). A
    /// message never edited has no history.
    pub fn new(skipped: usize, oldest_first: Vec<(Option<&str>, DateTime<Utc>)>) -> Self {
        if skipped + oldest_first.len() < 2 {
            return Self::default();
        }
        let older = skipped + oldest_first.len().saturating_sub(MAX_VERSIONS);
        let versions = oldest_first
            .into_iter()
            .rev()
            .take(MAX_VERSIONS)
            .map(|(source, at)| Version {
                markdown: source.map(|source| Arc::new(markdown::parse(source))),
                at,
            })
            .collect();
        Self { versions, older }
    }
}

/// How far deleting the account got when the core stopped waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountDeletion {
    /// Deleted: this device resets.
    Completed,
    /// Goes on in the background (also after a restart); this device
    /// resets once it is done.
    Pending,
}

/// One invite code, or a token to trade for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteSlot {
    Code {
        code: SharedString,
        /// Given out: copied once.
        copied: bool,
    },
    /// The core's id for the token.
    Token(i64),
}

/// Longest invite code shown or copied; the server's are 8 characters.
const MAX_INVITE_CODE: usize = 32;

/// An invite code as the server sent it, if it is only letters and
/// digits (bounded): what is shown and copied can't carry anything else,
/// and is exactly the code the core stored (it is marked by it). Any
/// other is left out, never altered: a changed code wouldn't work.
pub fn invite_code(code: &str) -> Option<SharedString> {
    let plain = !code.is_empty()
        && code.len() <= MAX_INVITE_CODE
        && code.chars().all(|c| c.is_ascii_alphanumeric());
    plain.then(|| code.to_string().into())
}

/// What copying invite codes puts on the clipboard (upstream's wording,
/// without its install link: that is for its own app).
pub fn invite_message(codes: &[SharedString]) -> String {
    match codes {
        [code] => format!("Let's chat on Air. Use invite code {code}."),
        codes => format!(
            "Let's chat on Air. Use one of these invite codes to join:\n\n{}",
            codes.join("\n")
        ),
    }
}

/// The account's settings the core syncs to the user's devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncedSettings {
    /// Others see when the user read their messages, and the user theirs.
    pub read_receipts: bool,
    /// Contact requests that expired are deleted, not kept as a record.
    pub delete_expired_requests: bool,
}

impl Default for SyncedSettings {
    /// The core's defaults for unset settings.
    fn default() -> Self {
        Self {
            read_receipts: true,
            delete_expired_requests: false,
        }
    }
}

/// How long a chat is muted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mute {
    Until(DateTime<Utc>),
    Forever,
}

/// Longest mute with a duration; longer is `forever`.
const MAX_MUTE_DAYS: i64 = 366;

impl Mute {
    pub fn is_muted(self, now: DateTime<Utc>) -> bool {
        match self {
            Mute::Until(until) => now < until,
            Mute::Forever => true,
        }
    }

    /// `:mute`'s argument at `now` (local time decides "tomorrow" and
    /// "monday"): nothing, `forever` or `always`; `tomorrow` (until
    /// midnight) or `monday` (until next Monday starts), as upstream's
    /// choices; or a duration like `30m`, `8h`, `2d`, `1w`.
    pub fn parse<Tz: chrono::TimeZone>(arg: &str, now: DateTime<Tz>) -> Result<Mute, &'static str> {
        const USAGE: &str = "Mute for a time like 30m, 8h, 2d, 1w, or tomorrow, monday, forever";
        let arg = arg.trim().to_lowercase();
        let days_ahead = match arg.as_str() {
            "" | "forever" | "always" => return Ok(Mute::Forever),
            "tomorrow" => Some(1),
            "monday" => {
                // A Monday mutes until the next one, as upstream.
                let today = i64::from(now.weekday().num_days_from_monday());
                Some(7 - today)
            }
            _ => None,
        };
        if let Some(days) = days_ahead {
            let day = now.date_naive() + chrono::Duration::days(days);
            let midnight = day.and_time(chrono::NaiveTime::MIN);
            // A midnight skipped by a clock change starts the day later.
            let start = now
                .timezone()
                .from_local_datetime(&midnight)
                .earliest()
                .or_else(|| {
                    now.timezone()
                        .from_local_datetime(&(midnight + chrono::Duration::hours(1)))
                        .earliest()
                })
                .ok_or(USAGE)?;
            return Ok(Mute::Until(start.with_timezone(&Utc)));
        }
        let split = arg.find(|c: char| !c.is_ascii_digit()).ok_or(USAGE)?;
        let (count, unit) = arg.split_at(split);
        let count: i64 = count.parse().map_err(|_| USAGE)?;
        let minutes = match unit {
            "m" | "min" => 1,
            "h" => 60,
            "d" => 24 * 60,
            "w" => 7 * 24 * 60,
            _ => return Err(USAGE),
        };
        let minutes = count.checked_mul(minutes).filter(|m| *m > 0).ok_or(USAGE)?;
        if minutes > MAX_MUTE_DAYS * 24 * 60 {
            return Err("That's over a year: mute forever instead");
        }
        Ok(Mute::Until(
            now.with_timezone(&Utc) + chrono::Duration::minutes(minutes),
        ))
    }

    /// What muting says, e.g. "Muted until 14:05" (local time; the date
    /// too when it isn't today).
    pub fn label(self, now: DateTime<Utc>) -> String {
        match self {
            Mute::Forever => "Muted".to_string(),
            Mute::Until(until) => {
                let (until, now) = (until.with_timezone(&Local), now.with_timezone(&Local));
                if until.date_naive() == now.date_naive() {
                    format!("Muted until {}", until.format("%H:%M"))
                } else {
                    format!("Muted until {}", until.format("%a %b %-d, %H:%M"))
                }
            }
        }
    }
}

/// Whether the message at `ix` starts a new visual group: its sender
/// differs from the previous message's, or either is an event.
pub fn starts_group(messages: &[Message], ix: usize) -> bool {
    match ix.checked_sub(1).and_then(|prev| messages.get(prev)) {
        Some(prev) => {
            let message = &messages[ix];
            prev.is_event() || message.is_event() || prev.sender_id != message.sender_id
        }
        None => true,
    }
}

/// Whether the message at `ix` is the first of its (local) day, so a date
/// divider goes above it.
pub fn starts_day(messages: &[Message], ix: usize) -> bool {
    let day = |message: &Message| message.sent_at.with_timezone(&Local).date_naive();
    match ix.checked_sub(1).and_then(|prev| messages.get(prev)) {
        Some(prev) => day(prev) != day(&messages[ix]),
        None => true,
    }
}

/// The countdown to a contact request's expiry, `None` once it has passed.
/// As upstream: rounded up, so it shows the most that could still be left,
/// and never "24 hours" or "60 minutes" (the next unit takes over).
pub fn expires_label(expires: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    let remaining = expires - now;
    let in_units = |count: i64, unit: &str| {
        let plural = if count == 1 { "" } else { "s" };
        format!("Expires in {count} {unit}{plural}")
    };
    let round_up = |unit: chrono::Duration| {
        let (remaining, unit) = (remaining.num_milliseconds(), unit.num_milliseconds());
        (remaining + unit - 1) / unit
    };
    if remaining <= chrono::Duration::zero() {
        None
    } else if remaining < chrono::Duration::minutes(1) {
        Some("Expires in less than a minute".into())
    } else if remaining < chrono::Duration::hours(1) {
        Some(match round_up(chrono::Duration::minutes(1)) {
            60.. => in_units(1, "hour"),
            minutes => in_units(minutes, "minute"),
        })
    } else if remaining < chrono::Duration::days(1) {
        Some(match round_up(chrono::Duration::hours(1)) {
            24.. => in_units(1, "day"),
            hours => in_units(hours, "hour"),
        })
    } else {
        Some(in_units(round_up(chrono::Duration::days(1)), "day"))
    }
}

/// Most emoji a message can have and still show large. As upstream.
const MAX_JUMBO_EMOJI: usize = 5;

/// Longest message that can show large: drawn as it came, on the UI thread
/// at every redraw, so a peer's text must not reach layout unbounded.
const MAX_JUMBO_BYTES: usize = 256;

/// Whether a message is only a few emoji on one line, which show large.
/// Spaces only: a line break (or any other whitespace) makes it a plain
/// message, whose rendering has its own limits.
pub fn is_jumbo_emoji(source: &str) -> bool {
    use unicode_segmentation::UnicodeSegmentation as _;
    if source.len() > MAX_JUMBO_BYTES {
        return false;
    }
    let mut count = 0;
    for grapheme in source.graphemes(true).filter(|g| *g != " ") {
        count += 1;
        if count > MAX_JUMBO_EMOJI || emojis::get(grapheme).is_none() {
            return false;
        }
    }
    count > 0
}

/// A date divider's label: "Today", "Yesterday", or the date.
pub fn format_day(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let at = at.with_timezone(&Local);
    let now = now.with_timezone(&Local);
    match (now.date_naive() - at.date_naive()).num_days() {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        _ if at.year() == now.year() => at.format("%A, %B %-d").to_string(),
        _ => at.format("%B %-d, %Y").to_string(),
    }
}

/// Index one step from `current` in a list of `len`, wrapping around. With
/// no current item, forward starts at the top and backward at the bottom.
pub fn step_index(len: usize, current: Option<usize>, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(ix), true) => (ix + 1) % len,
        (Some(ix), false) => (ix + len - 1) % len,
    })
}

/// When the soonest of the chats' timed mutes ends, after `now`.
pub fn next_mute_expiry(chats: &[ChatSummary], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    chats
        .iter()
        .filter_map(|chat| match chat.muted {
            Some(Mute::Until(until)) if until > now => Some(until),
            _ => None,
        })
        .min()
}

/// The nearest other chat with unread messages in the given direction,
/// wrapping around. Blocked chats show no unread count, so never one.
pub fn next_unread(chats: &[ChatSummary], current: Option<usize>, forward: bool) -> Option<usize> {
    let mut ix = current;
    for _ in 0..chats.len() {
        ix = step_index(chats.len(), ix, forward);
        let candidate = ix?;
        if Some(candidate) == current {
            break;
        }
        if chats[candidate].unread > 0 && !chats[candidate].blocked() {
            return Some(candidate);
        }
    }
    None
}

/// Moves a message cursor by `delta`, clamped to the list. With no cursor,
/// any movement starts at the newest (last) message, which is where the
/// view is scrolled to.
pub fn move_cursor(len: usize, cursor: Option<usize>, delta: isize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match cursor {
        None => last,
        Some(ix) => ix.saturating_add_signed(delta).min(last),
    })
}

/// Back/forward history of opened chats (Helix's jumplist: `ctrl-o`/`ctrl-i`).
#[derive(Debug, Default)]
pub struct JumpList {
    entries: Vec<ChatId>,
    /// Index of the current entry; meaningless while `entries` is empty.
    pos: usize,
}

impl JumpList {
    const MAX: usize = 100;

    /// Records a newly opened chat, dropping any forward history.
    pub fn push(&mut self, id: ChatId) {
        if self.entries.get(self.pos) == Some(&id) {
            return;
        }
        self.entries.truncate(self.pos + 1);
        self.entries.push(id);
        if self.entries.len() > Self::MAX {
            self.entries.remove(0);
        }
        self.pos = self.entries.len() - 1;
    }

    /// Drops entries for chats that are gone, so a jump never lands on
    /// nothing. If the current entry goes, the next newer one is current.
    pub fn retain(&mut self, mut keep: impl FnMut(&ChatId) -> bool) {
        let mut entries: Vec<ChatId> = Vec::with_capacity(self.entries.len());
        let mut pos = None;
        for (ix, id) in self.entries.drain(..).enumerate() {
            if !keep(&id) {
                continue;
            }
            // Pruning can bring a chat next to itself: a jump between the
            // two would go nowhere.
            if entries.last() != Some(&id) {
                entries.push(id);
            }
            if ix >= self.pos && pos.is_none() {
                pos = Some(entries.len() - 1);
            }
        }
        self.pos = pos.unwrap_or(entries.len().saturating_sub(1));
        self.entries = entries;
    }

    pub fn back(&mut self) -> Option<ChatId> {
        self.pos = self.pos.checked_sub(1)?;
        self.entries.get(self.pos).cloned()
    }

    pub fn forward(&mut self) -> Option<ChatId> {
        let next = self.entries.get(self.pos + 1)?.clone();
        self.pos += 1;
        Some(next)
    }
}

/// Chat-list timestamp: time today, "Yesterday", weekday within a week,
/// otherwise the date.
pub fn format_activity(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let at = at.with_timezone(&Local);
    let now = now.with_timezone(&Local);
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        ..=0 => at.format("%H:%M").to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => at.format("%a").to_string(),
        _ if at.year() == now.year() => at.format("%b %-d").to_string(),
        _ => at.format("%Y-%m-%d").to_string(),
    }
}

/// Message timestamp shown in a bubble.
pub fn format_message_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%H:%M").to_string()
}

/// Initials for an avatar placeholder: the first visible grapheme of the
/// first two words. Names are peer-controlled, so invisible format and
/// control characters (e.g. bidi overrides) are ignored.
pub fn initials(name: &str) -> String {
    let visible: String = name.chars().filter(|c| !is_invisible(*c)).collect();
    visible
        .split_whitespace()
        .filter_map(|word| word.graphemes(true).next())
        .take(2)
        .map(|grapheme| {
            // Keep graphemes whose uppercase form would change length
            // (e.g. ß → SS), so each word contributes exactly one grapheme.
            let upper = grapheme.to_uppercase();
            if upper.graphemes(true).count() == 1 {
                upper
            } else {
                grapheme.to_string()
            }
        })
        .collect()
}

fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}'                      // soft hyphen
            | '\u{034F}'                     // combining grapheme joiner
            | '\u{061C}'                     // Arabic letter mark
            | '\u{115F}' | '\u{1160}'         // Hangul fillers (blank "names")
            | '\u{180E}'                     // Mongolian vowel separator
            | '\u{200B}'                     // zero-width space (ZWJ/ZWNJ kept: emoji)
            | '\u{200E}' | '\u{200F}'         // LRM/RLM
            | '\u{202A}'..='\u{202E}'       // bidi embeddings/overrides
            | '\u{2060}'..='\u{2069}'       // word joiner, bidi isolates
            | '\u{3164}' | '\u{FFA0}'         // Hangul fillers
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}') // tag characters
}

pub mod mock {
    use chrono::Duration;

    use super::*;

    pub fn chats() -> Vec<ChatSummary> {
        let now = Utc::now();
        let chat = |id: &'static str,
                    title: &'static str,
                    group,
                    last: &'static str,
                    ago: Duration,
                    unread| {
            ChatSummary {
                id: ChatId(id.into()),
                title: title.into(),
                is_group: group,
                request: None,
                request_expires: None,
                is_self_chat: false,
                one_to_one: (!group).then_some(OneToOne::CONTACT),
                preview: Some(Preview {
                    prefix: None,
                    text: last.into(),
                    kind: PreviewKind::Message,
                }),
                last_activity: Some(now - ago),
                unread,
                muted: None,
                group_ended: false,
                picture: None,
            }
        };
        vec![
            chat(
                "ada",
                "Ada Lovelace",
                false,
                "The engine can compose music too.",
                Duration::minutes(3),
                2,
            ),
            chat(
                "design",
                "Design Crew",
                true,
                "Shipped the new palette 🎨",
                Duration::minutes(18),
                5,
            ),
            chat(
                "grace",
                "Grace Hopper",
                false,
                "Found the bug. It was a moth.",
                Duration::hours(3),
                0,
            ),
            chat(
                "alan",
                "Alan Turing",
                false,
                "Can machines think?",
                Duration::days(1),
                0,
            ),
            chat(
                "rust",
                "Rustaceans",
                true,
                "New release is out",
                Duration::days(4),
                0,
            ),
        ]
    }

    pub fn messages(chat: &ChatId) -> Vec<Message> {
        let peer_name: SharedString = match chat.0.as_ref() {
            "ada" => "Ada Lovelace",
            "grace" => "Grace Hopper",
            "alan" => "Alan Turing",
            _ => "Someone",
        }
        .into();
        let now = Utc::now();
        let mut next_id = 0;
        let mut msg = |own: bool, body: &'static str, minutes_ago: i64| {
            next_id += 1;
            let sender_id = UserId(if own { "me" } else { "peer" }.into());
            let sender_name = if own { "You".into() } else { peer_name.clone() };
            Message::text(
                MessageId(format!("{}-{next_id}", chat.0).into()),
                (sender_id, sender_name, own),
                body,
                now - Duration::minutes(minutes_ago),
            )
        };
        vec![
            msg(
                false,
                "Hey! Did you get a chance to look at the new client?",
                10,
            ),
            msg(true, "Yes, it is so much nicer on a big screen.", 9),
            msg(true, "And everything is reachable from the keyboard.", 9),
            Message {
                reactions: reactions([("👍", &["me", "peer"][..]), ("😂", &["peer"][..])], &"me").0,
                ..msg(false, "Ctrl+K to jump between chats is my favourite.", 7)
            },
            msg(false, "The engine can compose music too.", 3),
            Message {
                body: MessageBody::Attachment(Attachment {
                    id: AttachmentId(uuid::Uuid::nil()),
                    name: "notes-on-the-engine.pdf".into(),
                    description: None,
                    size: 248_000,
                    state: AttachmentState::Ready,
                    image: None,
                    animation: None,
                }),
                ..msg(false, "", 2)
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone as _};

    use super::*;

    #[test]
    fn a_request_counts_down_rounding_up() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let label = |left: Duration| expires_label(now + left, now);
        assert_eq!(label(Duration::zero()), None);
        assert_eq!(label(Duration::seconds(-1)), None);
        let says = |what: &str| Some(what.to_string());
        assert_eq!(
            label(Duration::seconds(59)),
            says("Expires in less than a minute")
        );
        assert_eq!(label(Duration::seconds(60)), says("Expires in 1 minute"));
        assert_eq!(label(Duration::seconds(61)), says("Expires in 2 minutes"));
        assert_eq!(
            label(Duration::milliseconds(60_500)),
            says("Expires in 2 minutes")
        );
        // Rounding up reads as the next unit, never "60 minutes".
        assert_eq!(label(Duration::seconds(3599)), says("Expires in 1 hour"));
        assert_eq!(label(Duration::hours(1)), says("Expires in 1 hour"));
        assert_eq!(label(Duration::seconds(86399)), says("Expires in 1 day"));
        assert_eq!(label(Duration::days(30)), says("Expires in 30 days"));
    }

    #[test]
    fn only_a_few_emoji_show_large() {
        for jumbo in ["😀", " 😀 😀 ", "😀😀😀😀😀", "👍🏽", "👨‍👩‍👧‍👦", "🇸🇪"]
        {
            assert!(is_jumbo_emoji(jumbo), "{jumbo}");
        }
        for plain in ["", "  ", "hi", "😀 hi", "😀😀😀😀😀😀", "*😀*", "1"] {
            assert!(!is_jumbo_emoji(plain), "{plain}");
        }
        // A peer's text reaches layout only bounded, and on one line.
        for huge in [
            format!("😀{}😀", "\n".repeat(1_000_000)),
            format!("😀{}😀", " ".repeat(1_000_000)),
            "😀\n😀".to_string(),
            "😀\u{2028}😀".to_string(),
        ] {
            assert!(!is_jumbo_emoji(&huge), "{}", huge.len());
        }
    }

    #[test]
    fn a_limited_animation_stops_after_its_loops() {
        use std::time::Duration as Span;
        let animation = Animation {
            picture: Picture(Arc::new(gpui_kit::RenderImage::new(Vec::new()))),
            loop_time: Span::from_secs(2),
        };
        assert!(animation.plays(Span::from_millis(5999), true));
        assert!(!animation.plays(Span::from_secs(6), true));
        assert!(animation.plays(Span::from_secs(600), false));

        // Slow frames don't keep the window redrawing for days.
        let slow = Animation {
            loop_time: Span::from_secs(157_000),
            ..animation.clone()
        };
        assert!(slow.plays(Span::from_secs(29), true));
        assert!(!slow.plays(Span::from_secs(30), true));
    }

    fn message(sender_id: &str, sender_name: &str) -> Message {
        let sender = (
            UserId(sender_id.to_string().into()),
            sender_name.to_string().into(),
            false,
        );
        Message::text(MessageId("m".into()), sender, "hi", Utc::now())
    }

    #[test]
    fn only_the_users_failed_upload_is_sent_again() {
        let file = |own, state| {
            let mut message = message("ada", "Ada");
            message.own = own;
            message.body = MessageBody::Attachment(Attachment {
                id: AttachmentId(uuid::Uuid::nil()),
                name: "f".into(),
                description: None,
                size: 1,
                state,
                image: None,
                animation: None,
            });
            message
        };
        assert!(file(true, AttachmentState::UploadFailed).upload_failed());
        // A peer's (whatever it shows), one still uploading, or text: no.
        assert!(!file(false, AttachmentState::UploadFailed).upload_failed());
        assert!(!file(true, AttachmentState::Uploading).upload_failed());
        assert!(!file(true, AttachmentState::DownloadFailed).upload_failed());
        assert!(!message("ada", "Ada").upload_failed());
    }

    #[test]
    fn grouping_uses_sender_id_not_display_name() {
        let messages = [
            message("alice", "Alice"),
            message("alice", "Alice"),
            // Impersonator copying Alice's display name must start a new group.
            message("mallory", "Alice"),
            // A peer calling themselves "You" is just another sender.
            message("eve", "You"),
            message("eve", "You"),
        ];
        let groups: Vec<_> = (0..messages.len())
            .map(|ix| starts_group(&messages, ix))
            .collect();
        assert_eq!(groups, [true, false, true, true, false]);

        // An event between two messages of one sender splits the group.
        let mut messages = vec![message("alice", "Alice"), message("alice", "Alice")];
        messages.insert(
            1,
            Message {
                sender_id: Some(UserId("alice".into())),
                body: MessageBody::Event("Alice changed the title".into()),
                ..message("alice", "Alice")
            },
        );
        let groups: Vec<_> = (0..3).map(|ix| starts_group(&messages, ix)).collect();
        assert_eq!(groups, [true, true, true]);
    }

    #[test]
    fn step_index_wraps_both_ways() {
        assert_eq!(step_index(0, None, true), None);
        assert_eq!(step_index(3, None, true), Some(0));
        assert_eq!(step_index(3, None, false), Some(2));
        assert_eq!(step_index(3, Some(2), true), Some(0));
        assert_eq!(step_index(3, Some(0), false), Some(2));
    }

    #[test]
    fn invite_codes_are_only_letters_and_digits() {
        assert_eq!(invite_code("AB12cd34"), Some("AB12cd34".into()));
        // A server can't slip text, lines or bidi tricks into the
        // clipboard; such a code is left out, not changed into another.
        for bad in ["AB\u{202E}12", "AB12\nhttp://x", "", "AB 12"] {
            assert_eq!(invite_code(bad), None, "{bad:?}");
        }
        assert_eq!(invite_code(&"A".repeat(33)), None);
        assert!(invite_code(&"A".repeat(32)).is_some());
        assert_eq!(
            invite_message(&["AB12".into()]),
            "Let's chat on Air. Use invite code AB12."
        );
        assert!(invite_message(&["AB12".into(), "CD34".into()]).ends_with("AB12\nCD34"));
    }

    #[test]
    fn each_deletion_applies_only_where_it_may() {
        let own_sent = MessageFacts {
            content: true,
            own: true,
            sent: true,
            deleted: false,
            erasable: true,
        };
        let chat = ChatFacts {
            self_chat: false,
            live: true,
        };
        let everyone = |message, chat| DeleteFor::Everyone.check(message, chat);
        assert_eq!(everyone(own_sent, chat), Ok(()));
        let refused = [
            MessageFacts {
                own: false,
                ..own_sent
            },
            MessageFacts {
                sent: false,
                ..own_sent
            },
            MessageFacts {
                deleted: true,
                ..own_sent
            },
            MessageFacts {
                content: false,
                ..own_sent
            },
        ];
        for message in refused {
            assert!(everyone(message, chat).is_err(), "{message:?}");
        }
        assert!(
            everyone(
                own_sent,
                ChatFacts {
                    self_chat: true,
                    ..chat
                }
            )
            .is_err()
        );
        assert!(
            everyone(
                own_sent,
                ChatFacts {
                    live: false,
                    ..chat
                }
            )
            .is_err()
        );

        // For the user: anything the core lets go, whoever sent it.
        let peers = MessageFacts {
            own: false,
            sent: true,
            ..own_sent
        };
        let dead = ChatFacts {
            self_chat: true,
            live: false,
        };
        assert_eq!(DeleteFor::Me.check(peers, dead), Ok(()));
        let kept = MessageFacts {
            content: false,
            erasable: false,
            ..peers
        };
        assert!(DeleteFor::Me.check(kept, chat).is_err());
    }

    #[test]
    fn mutes_parse_from_durations_and_days() {
        // Wednesday, 10:30 at UTC+2.
        let zone = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 4, 10, 30, 0).unwrap();
        let at = |d, h, m| {
            Mute::Until(
                zone.with_ymd_and_hms(2026, 11, d, h, m, 0)
                    .unwrap()
                    .with_timezone(&Utc),
            )
        };
        assert_eq!(Mute::parse("", now), Ok(Mute::Forever));
        assert_eq!(Mute::parse(" Always ", now), Ok(Mute::Forever));
        assert_eq!(Mute::parse("30m", now), Ok(at(4, 11, 0)));
        assert_eq!(Mute::parse("8h", now), Ok(at(4, 18, 30)));
        assert_eq!(Mute::parse("2d", now), Ok(at(6, 10, 30)));
        assert_eq!(Mute::parse("1w", now), Ok(at(11, 10, 30)));
        // Local midnights.
        assert_eq!(Mute::parse("tomorrow", now), Ok(at(5, 0, 0)));
        assert_eq!(Mute::parse("monday", now), Ok(at(9, 0, 0)));
        let monday = zone.with_ymd_and_hms(2026, 11, 9, 9, 0, 0).unwrap();
        assert_eq!(
            Mute::parse("monday", monday),
            Ok(Mute::Until(
                zone.with_ymd_and_hms(2026, 11, 16, 0, 0, 0)
                    .unwrap()
                    .with_timezone(&Utc)
            ))
        );
        for bad in [
            "0h",
            "-1h",
            "h",
            "5",
            "5y",
            "soon",
            "53w",
            "99999999999999999999m",
        ] {
            assert!(Mute::parse(bad, now).is_err(), "{bad}");
        }
        assert!(Mute::parse("52w", now).is_ok());
    }

    #[test]
    fn the_list_redraws_when_the_soonest_mute_ends() {
        let now = Utc::now();
        let chat = |muted| ChatSummary {
            muted,
            ..mock::chats().remove(0)
        };
        let soon = now + Duration::minutes(5);
        let later = now + Duration::hours(5);
        let chats = [
            chat(None),
            chat(Some(Mute::Forever)),
            chat(Some(Mute::Until(now - Duration::minutes(1)))),
            chat(Some(Mute::Until(later))),
            chat(Some(Mute::Until(soon))),
        ];
        assert_eq!(next_mute_expiry(&chats, now), Some(soon));
        assert_eq!(next_mute_expiry(&chats[..3], now), None);
        // The list's tag goes when the mute ends, not a moment later.
        assert!(chats[4].is_muted(now) && !chats[4].is_muted(soon));
    }

    #[test]
    fn edit_history_is_newest_first_and_bounded() {
        let at = |n: i64| DateTime::from_timestamp(n, 0).unwrap();
        assert_eq!(
            EditHistory::new(0, vec![(Some("only"), at(0))]),
            EditHistory::default()
        );
        let history = EditHistory::new(
            0,
            vec![(Some("first"), at(0)), (None, at(1)), (Some("now"), at(2))],
        );
        let times: Vec<_> = history.versions.iter().map(|v| v.at).collect();
        assert_eq!(times, [at(2), at(1), at(0)]);
        assert!(history.versions[1].markdown.is_none());
        assert_eq!(history.older, 0);
        let many = EditHistory::new(0, (0..25).map(|n| (Some("x"), at(n))).collect());
        assert_eq!(many.versions.len(), MAX_VERSIONS);
        assert_eq!(many.older, 5);
        assert_eq!(many.versions[0].at, at(24));
        // As the session passes them: the oldest left out unconverted.
        let kept = EditHistory::new(980, (980..1000).map(|n| (Some("x"), at(n))).collect());
        assert_eq!(kept.older, 980);
        assert_eq!(kept.versions.len(), MAX_VERSIONS);
        assert_eq!(kept.versions[0].at, at(999));
    }

    #[test]
    fn each_contact_operation_applies_only_where_it_may() {
        use ContactOp::*;
        let chat = |side, stage| OneToOne {
            side,
            stage,
            peer_known: true,
        };
        let contact = OneToOne::CONTACT;
        let blocked = chat(Side::Contact, Stage::Blocked);
        let incoming = chat(Side::Incoming, Stage::Open);
        let blocked_request = chat(Side::Incoming, Stage::Blocked);
        let outgoing = chat(Side::Outgoing, Stage::Open);
        let sending = chat(Side::Outgoing, Stage::Sending);
        let by_name = OneToOne {
            peer_known: false,
            ..outgoing
        };
        let allowed = |op: ContactOp, chat| op.check(chat).is_ok();

        // A contact: block once, unblock only when blocked, report, delete.
        assert!(allowed(Block, contact) && !allowed(Block, blocked));
        assert!(!allowed(Unblock, contact) && allowed(Unblock, blocked));
        assert!(allowed(Report, contact) && !allowed(Retract, contact));
        assert!(allowed(Delete, contact) && allowed(Delete, blocked));
        // An open incoming request: blocking declines it; deleting it is
        // declining, which has its own command. Once blocked, it can be
        // unblocked (the block outlives the request) or deleted.
        assert!(allowed(Block, incoming));
        assert!(!allowed(Delete, incoming) && !allowed(Retract, incoming));
        assert!(allowed(Unblock, blocked_request) && allowed(Delete, blocked_request));
        assert_eq!(blocked_request.request(), Some(Request::Closed));
        // An open outgoing request: retracted, not blocked or deleted, and
        // not before it is sent.
        assert!(allowed(Retract, outgoing) && allowed(Retract, by_name));
        assert!(!allowed(Retract, sending));
        assert!(!allowed(Block, outgoing) && !allowed(Delete, outgoing));
        // Sent to a username: nobody known to report.
        assert!(!allowed(Report, by_name) && allowed(Report, outgoing));
        // Ended requests are only a record, deleted like a chat.
        for side in [Side::Incoming, Side::Outgoing] {
            let ended = chat(side, Stage::Ended);
            assert!(allowed(Delete, ended) && !allowed(Retract, ended));
            assert_eq!(ended.request(), Some(Request::Closed));
        }
        assert_eq!(contact.request(), None);
        // A request to someone blocked is deleted, not retracted.
        let blocked_outgoing = chat(Side::Outgoing, Stage::Blocked);
        assert!(!allowed(Retract, blocked_outgoing) && allowed(Delete, blocked_outgoing));
    }

    #[test]
    fn pictures_show_for_groups_and_established_contacts_only() {
        let chat = |side, stage| {
            Some(OneToOne {
                side,
                stage,
                peer_known: true,
            })
        };
        assert!(shows_picture(None, false));
        assert!(!shows_picture(None, true), "the self chat has its own mark");
        assert!(shows_picture(chat(Side::Contact, Stage::Open), false));
        assert!(shows_picture(chat(Side::Contact, Stage::Ended), false));
        // A requester's picture waits for acceptance; a blocked one's goes.
        assert!(!shows_picture(chat(Side::Incoming, Stage::Open), false));
        assert!(!shows_picture(chat(Side::Outgoing, Stage::Sending), false));
        assert!(!shows_picture(chat(Side::Contact, Stage::Blocked), false));
    }

    #[test]
    fn only_a_live_chat_is_written_to() {
        let chat = |one_to_one: Option<OneToOne>, request| ChatSummary {
            id: ChatId("c".into()),
            title: "chat".into(),
            is_group: one_to_one.is_none(),
            request,
            request_expires: None,
            is_self_chat: false,
            one_to_one,
            preview: None,
            last_activity: None,
            unread: 0,
            muted: None,
            group_ended: false,
            picture: None,
        };
        let stage = |stage| {
            Some(OneToOne {
                stage,
                ..OneToOne::CONTACT
            })
        };
        assert!(chat(None, None).composes(), "a group");
        assert!(chat(stage(Stage::Open), None).composes());
        let ended = chat(stage(Stage::Ended), None);
        assert!(!ended.composes() && ended.ended());
        assert!(!chat(stage(Stage::Blocked), None).composes());
        let incoming = OneToOne {
            side: Side::Incoming,
            ..OneToOne::CONTACT
        };
        assert!(!chat(Some(incoming), Some(Request::Incoming)).composes());
        // A group the user is out of takes nothing, and deleting a
        // message for everyone there is refused before asking.
        let out = ChatSummary {
            group_ended: true,
            ..chat(None, None)
        };
        assert!(!out.composes());
        let own = MessageFacts {
            content: true,
            own: true,
            sent: true,
            deleted: false,
            erasable: true,
        };
        assert!(
            DeleteFor::Everyone
                .check(own, out.deletion_facts())
                .is_err()
        );
        // The session's rule is the same one.
        assert!(!takes_messages(None, true));
        assert!(!takes_messages(stage(Stage::Ended), false));
        assert!(!takes_messages(Some(incoming), false));
        assert!(takes_messages(stage(Stage::Sending), false));
    }

    #[test]
    fn next_unread_skips_read_and_current_chats() {
        let chats: Vec<_> = [0, 3, 0, 1, 0]
            .into_iter()
            .enumerate()
            .map(|(ix, unread)| ChatSummary {
                id: ChatId(ix.to_string().into()),
                title: "chat".into(),
                is_group: false,
                request: None,
                request_expires: None,
                is_self_chat: false,
                one_to_one: None,
                preview: None,
                last_activity: None,
                unread,
                muted: None,
                group_ended: false,
                picture: None,
            })
            .collect();
        assert_eq!(next_unread(&chats, None, true), Some(1));
        assert_eq!(next_unread(&chats, Some(1), true), Some(3));
        assert_eq!(next_unread(&chats, Some(3), true), Some(1)); // wraps
        assert_eq!(next_unread(&chats, Some(1), false), Some(3)); // wraps back
        // The current chat alone being unread is not a target.
        assert_eq!(next_unread(&chats[..2], Some(1), true), None);
        assert_eq!(next_unread(&[], None, true), None);
        // A blocked chat's unread messages lead nowhere.
        let mut blocked = chats.clone();
        blocked[3].one_to_one = Some(OneToOne {
            stage: Stage::Blocked,
            ..OneToOne::CONTACT
        });
        assert_eq!(next_unread(&blocked, Some(1), true), None);
    }

    #[test]
    fn move_cursor_clamps_and_starts_at_newest() {
        assert_eq!(move_cursor(0, None, 1), None);
        assert_eq!(move_cursor(5, None, -1), Some(4));
        assert_eq!(move_cursor(5, None, 1), Some(4));
        assert_eq!(move_cursor(5, Some(4), 1), Some(4));
        assert_eq!(move_cursor(5, Some(1), -10), Some(0));
        assert_eq!(move_cursor(5, Some(1), 2), Some(3));
        // A cursor left past the end (list shrank) is pulled back in.
        assert_eq!(move_cursor(3, Some(7), 0), Some(2));
    }

    #[test]
    fn jump_list_back_forward_and_branching() {
        let id = |s: &str| ChatId(s.to_string().into());
        let mut jumps = JumpList::default();
        assert_eq!(jumps.back(), None);
        jumps.push(id("a"));
        jumps.push(id("b"));
        jumps.push(id("b")); // re-opening the current chat is not a jump
        jumps.push(id("c"));
        assert_eq!(jumps.back(), Some(id("b")));
        assert_eq!(jumps.back(), Some(id("a")));
        assert_eq!(jumps.back(), None);
        assert_eq!(jumps.forward(), Some(id("b")));
        // A new jump from the middle discards forward history.
        jumps.push(id("d"));
        assert_eq!(jumps.forward(), None);
        assert_eq!(jumps.back(), Some(id("b")));
    }

    #[test]
    fn jump_list_skips_chats_that_are_gone() {
        let id = |s: &str| ChatId(s.to_string().into());
        let mut jumps = JumpList::default();
        for chat in ["a", "b", "c", "d"] {
            jumps.push(id(chat));
        }
        jumps.back(); // at c
        // b was deleted elsewhere: back goes straight to a.
        jumps.retain(|chat| chat != &id("b"));
        assert_eq!(jumps.back(), Some(id("a")));
        assert_eq!(jumps.forward(), Some(id("c")));
        // The current chat itself went away: the newer one stands in.
        jumps.retain(|chat| chat != &id("c"));
        assert_eq!(jumps.back(), Some(id("a")));
        assert_eq!(jumps.forward(), Some(id("d")));
        jumps.retain(|_| false);
        assert_eq!((jumps.back(), jumps.forward()), (None, None));

        // a b a with b gone: one a, not a jump from a to a.
        let mut jumps = JumpList::default();
        for chat in ["a", "b", "a", "c"] {
            jumps.push(id(chat));
        }
        jumps.retain(|chat| chat != &id("b"));
        assert_eq!(jumps.back(), Some(id("a")));
        assert_eq!(jumps.back(), None);
    }

    #[test]
    fn jump_list_is_bounded() {
        let mut jumps = JumpList::default();
        for ix in 0..250 {
            jumps.push(ChatId(ix.to_string().into()));
        }
        let mut steps = 0;
        while jumps.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, JumpList::MAX - 1);
    }

    #[test]
    fn activity_format_by_age() {
        let now = Local
            .with_ymd_and_hms(2026, 11, 5, 15, 0, 0)
            .unwrap()
            .to_utc();
        let at = |delta: Duration| format_activity(now - delta, now);
        assert_eq!(at(Duration::hours(1)), "14:00");
        assert_eq!(at(Duration::days(1)), "Yesterday");
        assert_eq!(at(Duration::days(3)), "Mon");
        assert_eq!(at(Duration::days(30)), "Oct 6");
        assert_eq!(at(Duration::days(400)), "2025-10-01");
        // Clock skew: a peer timestamp in the future still renders as a time.
        assert_eq!(at(Duration::hours(-2)), "17:00");
    }

    #[test]
    fn reactions_are_bounded_and_never_hide_the_users_own() {
        let many: Vec<String> = (0..40)
            .map(|n| char::from_u32(0x1F600 + n).unwrap().to_string())
            .collect();
        const ME: &str = "me";
        const PEER: &str = "peer";
        const OTHER: &str = "other";
        let mut raw: Vec<(&str, &[&str])> =
            many.iter().map(|e| (e.as_str(), &[PEER][..])).collect();
        // Look like 👍 once invisible characters go: counted with it, and
        // the peer reacting with three variants counts once.
        raw.insert(0, ("👍", &[PEER, OTHER][..]));
        raw.insert(1, ("\u{200B}👍\u{202E}", &[ME, PEER][..]));
        raw.insert(2, ("👍\u{E0001}", &[PEER][..]));
        // The user's, but far down the list.
        raw.push(("🙏", &[ME][..]));
        // Text instead of an emoji, and nothing visible.
        raw.insert(3, ("not an emoji at all", &[PEER][..]));
        raw.insert(4, ("\u{200B}", &[PEER][..]));
        let (shown, more) = reactions(raw, &ME);
        assert_eq!(
            shown[0],
            Reaction {
                label: "👍".into(),
                count: 3,
                own: true
            }
        );
        assert_eq!(shown[1].label.as_ref(), "n…");
        assert_eq!(shown[2].label.as_ref(), "?");
        assert_eq!(shown.len(), MAX_REACTIONS + 1);
        assert!(shown.last().is_some_and(|r| r.label == "🙏" && r.own));
        assert_eq!(more, 3 + 40 + 1 - shown.len());
    }

    #[test]
    fn a_picker_key_retracts_only_what_the_user_gave() {
        // Every quick reaction labels as itself (❤️ keeps its selector).
        for emoji in QUICK_REACTIONS {
            assert_eq!(reaction_label(emoji).as_ref(), emoji);
        }
        let (shown, _) = reactions([("👍\u{200B}", &["me"][..]), ("😂", &["peer"][..])], &"me");
        assert!(gave(&shown, "👍"), "a variant of the user's counts");
        assert!(!gave(&shown, "😂"), "a peer's is added to, not retracted");
        assert!(!gave(&shown, "🙏"));
    }

    #[test]
    fn preview_lines_are_single_bounded_and_visible() {
        assert_eq!(preview_line("  two\n\nlines\t here "), "two lines here");
        // A bidi override can't reorder the row around it.
        assert_eq!(preview_line("\u{202E}evil\u{200B}"), "evil");
        let long = "é".repeat(10_000);
        let line = preview_line(&long);
        assert_eq!(line.graphemes(true).count(), MAX_PREVIEW + 1);
        assert!(line.ends_with('…'));
        // Grapheme clusters are never split.
        let flags = "🏳️‍🌈".repeat(MAX_PREVIEW + 5);
        assert!(preview_line(&flags).starts_with("🏳️‍🌈"));
        assert_eq!(
            preview_line(&flags).graphemes(true).count(),
            MAX_PREVIEW + 1
        );
    }

    #[test]
    fn names_are_one_short_line() {
        let title = format!("Lab\nnotes\u{202E}{}", "x".repeat(10_000));
        let name = name_line(&title);
        assert!(name.starts_with("Lab notesx"));
        assert_eq!(name.graphemes(true).count(), MAX_NAME + 1);
        assert_eq!(name_line("  Ada\u{200B} "), "Ada");
        // Combining marks can't inflate a name past its grapheme cap.
        let zalgo = format!("a{}", "\u{301}".repeat(5_000)).repeat(MAX_NAME);
        assert!(name_line(&zalgo).chars().count() <= MAX_NAME * MAX_GRAPHEME_CHARS);
        // Names made of fillers or marks alone would show as blank.
        assert_eq!(name_line("\u{3164}\u{115F} \u{061C}"), "Unnamed");
    }

    #[test]
    fn days_divide_by_local_date() {
        let at = |day, hour| {
            Local
                .with_ymd_and_hms(2026, 11, day, hour, 0, 0)
                .unwrap()
                .to_utc()
        };
        let messages: Vec<_> = [at(3, 23), at(4, 0), at(4, 9)]
            .into_iter()
            .map(|sent_at| Message {
                sent_at,
                ..message("alice", "Alice")
            })
            .collect();
        let starts: Vec<_> = (0..3).map(|ix| starts_day(&messages, ix)).collect();
        assert_eq!(starts, [true, true, false]);
        let now = at(5, 12);
        assert_eq!(format_day(at(5, 1), now), "Today");
        assert_eq!(format_day(at(4, 23), now), "Yesterday");
        assert_eq!(format_day(at(2, 9), now), "Monday, November 2");
    }

    #[test]
    fn initials_are_visible_graphemes() {
        assert_eq!(initials(""), "");
        assert_eq!(initials("  ada  "), "A");
        assert_eq!(initials("grace brewster hopper"), "GB");
        // Combining marks and ZWJ emoji stay whole.
        assert_eq!(initials("e\u{301}lodie x"), "E\u{301}X");
        assert_eq!(initials("👩‍💻 coder"), "👩‍💻C");
        // ß would uppercase to two letters; keep it as one grapheme.
        assert_eq!(initials("ßtraße"), "ß");
        // Bidi overrides and zero-width chars can't become the initial.
        assert_eq!(initials("\u{202E}mallory \u{200B}eve"), "ME");
        assert_eq!(initials("\u{202E}\u{200B}"), "");
    }

    #[test]
    fn saved_names_are_one_bounded_component_keeping_the_extension() {
        assert_eq!(file_name("report.pdf"), "report.pdf");
        assert_eq!(file_name("../../.ssh/id"), "_.._.ssh_id");
        assert_eq!(file_name("..."), "attachment");
        assert_eq!(file_name("\u{202E}\u{200B} "), "attachment");
        assert_eq!(file_name("C:\\x\ny"), "C__x y");
        let long = format!("{}.tar.gz", "ä".repeat(300));
        let cut = file_name(&long);
        assert!(cut.len() <= MAX_FILE_NAME, "{}", cut.len());
        assert!(cut.ends_with("ä.gz"), "{cut}");
        assert_eq!(numbered_file_name("a.tar.gz", 2), "a.tar (2).gz");
        assert_eq!(numbered_file_name("README", 3), "README (3)");
    }

    #[test]
    fn previews_fit_their_box_without_growing() {
        assert_eq!(fit((1024, 512), (360, 270)), (360, 180));
        assert_eq!(fit((100, 1000), (360, 270)), (27, 270));
        assert_eq!(fit((20, 10), (360, 270)), (20, 10));
        assert_eq!(fit((4000, 1), (360, 270)), (360, 1));
    }
}
