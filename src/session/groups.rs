//! Groups: create, invite, remove, rename, leave and delete, and the members
//! of an open group. Port of upstream's group operations on `UserCubit` and
//! `ChatDetailsCubit`, and of `MemberDetailsCubit` (members, and who the room
//! policy lets the user remove).
//!
//! Operations run detached on tokio, as contact operations do (see
//! `contacts::on_tokio`): the core's jobs clean up after themselves, so
//! closing a view mustn't stop them midway.

use std::collections::HashMap;

use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::DbEntityId;
use aircoreclient::{ChatId as CoreChatId, ChatStatus, ChatType};
use gpui::SharedString;
use gpui_kit::{App, Context, Entity, Subscription, Task};
use mimi_room_policy::{MimiProposal, RoleIndex, VerifiedRoomState};
use tls_codec::Serialize as _;
use tracing::error;

use super::chats::model_id;
use super::contacts::{Failure, on_tokio};
use super::messages::core_chat_id;
use super::{Session, SessionEvent};
use crate::command_line::{NameMatch, matches_name};
use crate::model::{ChatId, Member, UserId};
use crate::runtime::Tokio;

/// Longest group title accepted, in characters. Titles are shown as one
/// bounded line anyway; this keeps what is sent to the group modest.
const MAX_TITLE: usize = 128;

impl Session {
    /// Creates a group with only the user in it. Resolves to the new chat.
    pub fn create_group(
        &self,
        title: &str,
        cx: &App,
    ) -> impl Future<Output = Result<ChatId, String>> + use<> {
        let (user, title) = (self.user.clone(), title.to_string());
        on_tokio(cx, "create the group", async move {
            Ok(model_id(&create(&user, &title).await?))
        })
    }

    /// Invites the contact `query` names (see [`matches_name`]) to `chat`.
    /// Resolves to the contact's name.
    pub fn invite(
        &self,
        chat: &ChatId,
        query: &str,
        cx: &App,
    ) -> impl Future<Output = Result<SharedString, String>> + use<> {
        let (user, chat, query) = (self.user.clone(), core_chat_id(chat), query.to_string());
        on_tokio(cx, "invite to the group", invite(user, chat, query))
    }

    pub fn rename_group(
        &self,
        chat: &ChatId,
        title: &str,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat, title) = (self.user.clone(), core_chat_id(chat), title.to_string());
        on_tokio(cx, "rename the group", async move {
            rename(&user, chat, &title).await
        })
    }

    /// Leaves the group; it stays in the list, ended, until deleted.
    pub fn leave_group(
        &self,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(
            cx,
            "leave the group",
            async move { leave(&user, chat).await },
        )
    }

    /// Deletes the group here: leaves it if still in it, then erases it.
    pub fn delete_group(
        &self,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(
            cx,
            "delete the group",
            async move { delete(&user, chat).await },
        )
    }
}

pub(super) async fn create(user: &CoreUser, title: &str) -> Result<CoreChatId, Failure> {
    let title = parse_title(title)?;
    Ok(user.create_chat(title, None, false).await?)
}

pub(super) async fn rename(user: &CoreUser, chat: CoreChatId, title: &str) -> Result<(), Failure> {
    group(user, chat).await?;
    Ok(user.set_chat_title(chat, parse_title(title)?).await?)
}

pub(super) async fn leave(user: &CoreUser, chat: CoreChatId) -> Result<(), Failure> {
    let group = group(user, chat).await?;
    if matches!(group.status(), ChatStatus::Inactive(_)) {
        return Err(Failure::Shown("You're no longer in this group".into()));
    }
    Ok(user.leave_chat(chat).await?)
}

pub(super) async fn delete(user: &CoreUser, chat: CoreChatId) -> Result<(), Failure> {
    group(user, chat).await?;
    Ok(user.delete_and_erase_chat(chat).await?)
}

/// The chat, if it is a group. The self chat is one in the core (of the
/// user's devices), but never managed as one: leaving or inviting to it
/// would break device sync.
async fn group(user: &CoreUser, chat: CoreChatId) -> Result<aircoreclient::Chat, Failure> {
    let Some(group) = user.chat(&chat).await else {
        return Err(Failure::Shown("The chat is gone".into()));
    };
    let is_self = user
        .chat_is_self_group(&group.group_id)
        .await
        .map_err(Failure::Internal)?;
    if !matches!(group.chat_type, ChatType::Group(_)) || is_self {
        return Err(Failure::Shown("That works in groups only".into()));
    }
    Ok(group)
}

fn parse_title(title: &str) -> Result<String, Failure> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Failure::Shown("Groups need a name".into()));
    }
    if title.chars().count() > MAX_TITLE {
        return Err(Failure::Shown(format!(
            "Group names have at most {MAX_TITLE} characters"
        )));
    }
    Ok(title.to_string())
}

pub(super) async fn invite(
    user: CoreUser,
    chat: CoreChatId,
    query: String,
) -> Result<SharedString, Failure> {
    group(&user, chat).await?;
    let contacts = user
        .contacts_with_supported_features()
        .await
        .map_err(anyhow::Error::from)?;
    let mut named = Vec::with_capacity(contacts.len());
    for contact in contacts {
        let name = super::display_name(&user, &contact.user_id).await;
        named.push((name, contact));
    }
    // Names are peer-chosen and may imitate each other: never guess.
    let (name, contact) = match matches_name(&query, named.iter().map(|(name, _)| name.as_ref())) {
        NameMatch::One(name) => named
            .iter()
            .find(|(named, _)| named.as_ref() == name)
            .ok_or_else(|| Failure::Internal(anyhow::anyhow!("matched name vanished")))?,
        NameMatch::None => {
            return Err(Failure::Shown(format!("No contact matches {query:?}")));
        }
        NameMatch::Ambiguous(n) => {
            return Err(Failure::Shown(format!(
                "{n} contacts match {query:?}; be more specific"
            )));
        }
    };
    if user
        .chat_participants(chat)
        .await
        .is_some_and(|members| members.contains(&contact.user_id))
    {
        return Err(Failure::Shown(format!("{name} is already in the group")));
    }
    // As upstream: a contact whose features aren't known yet, or whose app
    // can't take part in groups, isn't invited.
    let supported = contact
        .supported_features
        .as_ref()
        .is_some_and(|features| features.encrypted_group_profiles);
    if !supported {
        return Err(Failure::Shown(format!(
            "{name}'s app can't join groups yet"
        )));
    }
    let result = user
        .invite_users(chat, std::slice::from_ref(&contact.user_id))
        .await?;
    if !result.users_not_added.is_empty() {
        return Err(Failure::Shown(format!(
            "{name}'s app can't join this group"
        )));
    }
    Ok(name.clone())
}

/// The members of one group, kept up to date.
pub struct Members {
    user: CoreUser,
    chat: CoreChatId,
    members: Vec<Member>,
    /// The core's ids behind the members shown (model ids drop the domain).
    ids: HashMap<UserId, CoreUserId>,
    loaded: bool,
    /// The latest load; a newer one replaces (cancels) it.
    load: Option<Task<()>>,
    _changes: Subscription,
}

impl Members {
    pub fn open(session: &Entity<Session>, chat: &ChatId, cx: &mut Context<Self>) -> Self {
        let chat = core_chat_id(chat);
        // As upstream: the chat (membership, roles) or any user (names).
        let changes = cx.subscribe(session, move |this: &mut Self, _, event, cx| match event {
            SessionEvent::Changed {
                notifications,
                overflowed,
            } => {
                let touched = notifications.iter().any(|notification| {
                    notification.ops.keys().any(|entity| match entity {
                        DbEntityId::Chat(id) => *id == chat,
                        DbEntityId::User(_) => true,
                        _ => false,
                    })
                });
                if *overflowed || touched {
                    this.reload(cx);
                }
            }
        });
        let mut members = Self {
            user: session.read(cx).user().clone(),
            chat,
            members: Vec::new(),
            ids: HashMap::new(),
            loaded: false,
            load: None,
            _changes: changes,
        };
        members.reload(cx);
        members
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Removes the member `query` names (see [`matches_name`]). Resolves to
    /// their name.
    pub fn remove(
        &self,
        query: &str,
        cx: &App,
    ) -> impl Future<Output = Result<SharedString, String>> + use<> {
        let target = self.removal(query);
        let (user, chat) = (self.user.clone(), self.chat);
        on_tokio(cx, "remove the member", async move {
            let (name, id) = target?;
            user.remove_users(chat, vec![id]).await?;
            Ok(name)
        })
    }

    /// Who `query` names, if the user may remove them.
    fn removal(&self, query: &str) -> Result<(SharedString, CoreUserId), Failure> {
        let others: Vec<_> = self.members.iter().filter(|member| !member.own).collect();
        let member = match matches_name(query, others.iter().map(|member| member.name.as_ref())) {
            NameMatch::One(name) => others.iter().find(|member| member.name.as_ref() == name),
            NameMatch::None => {
                return Err(Failure::Shown(format!("No member matches {query:?}")));
            }
            NameMatch::Ambiguous(n) => {
                return Err(Failure::Shown(format!(
                    "{n} members match {query:?}; be more specific"
                )));
            }
        };
        let Some((member, id)) = member.and_then(|m| Some((m, self.ids.get(&m.id)?.clone())))
        else {
            return Err(Failure::Shown("That member is gone".into()));
        };
        if !member.can_remove {
            return Err(Failure::Shown(format!(
                "You can't remove {} from this group",
                member.name
            )));
        }
        Ok((member.name.clone(), id))
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let (user, chat) = (self.user.clone(), self.chat);
        let load = Tokio::spawn(cx, async move { load(&user, chat).await });
        self.load = Some(cx.spawn(async move |this, cx| {
            let loaded = load.await;
            _ = this.update(cx, |this, cx| {
                match loaded {
                    Ok(Some(loaded)) => (this.members, this.ids) = loaded.into_iter().unzip(),
                    // The chat is gone; the workspace closes it.
                    Ok(None) => {}
                    Err(err) => error!("failed to load group members: {err:#}"),
                }
                this.loaded = true;
                cx.notify();
            });
        }));
    }
}

/// The group's members, the user first, then by name. `None` without a chat.
pub(super) async fn load(
    user: &CoreUser,
    chat: CoreChatId,
) -> Option<Vec<(Member, (UserId, CoreUserId))>> {
    let participants = user.chat_participants(chat).await?;
    let removing = user.pending_removes(chat).await.unwrap_or_default();
    let room = user
        .load_room_state(&chat)
        .await
        .inspect_err(|err| error!("failed to load a group's room state: {err:#}"))
        .ok();
    let mut members = Vec::with_capacity(participants.len());
    for id in participants {
        let own = &id == user.user_id();
        let model = UserId(id.uuid().to_string().into());
        let member = Member {
            id: model.clone(),
            name: super::display_name(user, &id).await,
            own,
            removing: removing.contains(&id),
            can_remove: !own
                && room
                    .as_ref()
                    .is_some_and(|(us, state)| can_remove(us, state, &id)),
        };
        members.push((member, (model, id)));
    }
    members.sort_by(|(a, _), (b, _)| (!a.own, &a.name).cmp(&(!b.own, &b.name)));
    Some(members)
}

/// Whether the room policy lets `us` make `target` an outsider: upstream's
/// `UiRoomState::can_kick`.
fn can_remove(us: &CoreUserId, state: &VerifiedRoomState, target: &CoreUserId) -> bool {
    let (Ok(us), Ok(target)) = (us.tls_serialize_detached(), target.tls_serialize_detached())
    else {
        return false;
    };
    state
        .can_apply_regular_proposals(
            &us,
            &[MimiProposal::ChangeRole {
                target,
                role: RoleIndex::Outsider,
            }],
        )
        .is_ok()
}
