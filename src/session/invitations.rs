//! Invite codes the user can give to people who want to join Air. Port of
//! upstream's `InvitationCodesCubit`.
//!
//! The server hands the account tokens over time; each can be traded for
//! one code. Codes the user copied count as used and can be cleared.

use aircoreclient::clients::CoreUser;
use aircoreclient::{RequestInvitationCodeError, TokenId};
use gpui_kit::{App, Context, Task};
use tracing::{error, warn};

use super::contacts::{Failure, on_tokio};
use crate::model::{InviteSlot, invite_code};
use crate::runtime::Tokio;

pub struct Invitations {
    user: CoreUser,
    pub(super) slots: Vec<InviteSlot>,
    /// The tokens behind the `Token` slots (the core needs them whole).
    tokens: Vec<TokenId>,
    /// The latest load; a newer one replaces (cancels) it.
    load: Option<Task<()>>,
}

pub(super) struct Loaded {
    pub(super) slots: Vec<InviteSlot>,
    tokens: Vec<TokenId>,
}

/// Codes first, oldest first, then the tokens not traded yet, as
/// upstream.
pub(super) async fn load(user: CoreUser) -> anyhow::Result<Loaded> {
    let mut codes = user.load_invitation_codes().await?;
    codes.sort_unstable_by(|a, b| a.created_at.cmp(&b.created_at).then(a.code.cmp(&b.code)));
    let mut tokens = user.load_invitation_token_ids().await?;
    tokens.sort_unstable_by_key(|token| (token.created_at, token.id));
    let slots = codes
        .into_iter()
        .filter_map(|code| {
            let shown = invite_code(&code.code);
            if shown.is_none() {
                warn!("left out an invite code that isn't plain letters and digits");
            }
            Some(InviteSlot::Code {
                code: shown?,
                copied: code.copied,
            })
        })
        .chain(tokens.iter().map(|token| InviteSlot::Token(token.id)))
        .collect();
    Ok(Loaded { slots, tokens })
}

impl Invitations {
    pub fn new(user: CoreUser, cx: &mut Context<Self>) -> Self {
        let mut invitations = Self {
            user,
            slots: Vec::new(),
            tokens: Vec::new(),
            load: None,
        };
        invitations.reload(cx);
        invitations
    }

    pub fn slots(&self) -> &[InviteSlot] {
        &self.slots
    }

    /// Loads the codes and tokens again: tokens arrive in the background,
    /// without the core telling.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let task = Tokio::spawn(cx, load(self.user.clone()));
        self.load = Some(cx.spawn(
            async move |this, cx| match task.await.and_then(|loaded| loaded) {
                Ok(loaded) => {
                    _ = this.update(cx, |this, cx| {
                        this.slots = loaded.slots;
                        this.tokens = loaded.tokens;
                        cx.notify();
                    });
                }
                Err(err) => error!("failed to load invite codes: {err:#}"),
            },
        ));
    }

    /// Trades the token `id` for a code. Resolves to the code.
    pub fn request(
        &self,
        id: i64,
        cx: &App,
    ) -> impl Future<Output = Result<gpui::SharedString, String>> + use<> {
        let user = self.user.clone();
        let token = self.tokens.iter().find(|token| token.id == id).cloned();
        on_tokio(cx, "get an invite code", async move {
            let token = token.ok_or_else(|| Failure::Shown("That slot is gone".into()))?;
            match user.request_invitation_code(token).await? {
                Ok(code) => invite_code(&code.code).ok_or_else(|| {
                    Failure::Internal(anyhow::anyhow!("the server sent an unusual invite code"))
                }),
                Err(RequestInvitationCodeError::GlobalQuotaExceeded) => Err(Failure::Shown(
                    "Air is out of invite codes for now. Try again later.".into(),
                )),
            }
        })
    }

    /// Marks `codes` as given out (copied).
    pub fn mark_copied(
        &self,
        codes: Vec<gpui::SharedString>,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let user = self.user.clone();
        let what = "mark the code as copied";
        on_tokio(cx, what, async move {
            for code in codes {
                user.mark_invitation_code_as_copied(&code)
                    .await
                    .map_err(|err| Failure::from(err).local(what))?;
            }
            Ok(())
        })
    }

    /// Forgets the codes given out.
    pub fn clear_copied(&self, cx: &App) -> impl Future<Output = Result<(), String>> + use<> {
        let user = self.user.clone();
        let what = "remove the used codes";
        on_tokio(cx, what, async move {
            user.clear_copied_codes()
                .await
                .map_err(|err| Failure::from(err).local(what))
        })
    }
}
