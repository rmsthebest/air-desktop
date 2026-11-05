//! Database changes a worker hasn't looked at yet, merged: a burst of
//! changes leads to one reload.

use std::sync::{Arc, Mutex, PoisonError};

use aircoreclient::db::notification::{DbEntityId, DbNotification};
use tokio::sync::Notify;

#[derive(Default)]
pub(super) struct Pending {
    changes: Mutex<Changes>,
    pub(super) wake: Notify,
}

#[derive(Default)]
pub(super) struct Changes {
    /// Every changed entity with the union of its operations.
    pub(super) merged: DbNotification,
    /// Changes were dropped: reload everything.
    pub(super) all: bool,
}

impl Changes {
    pub(super) fn entities(&self) -> impl Iterator<Item = &DbEntityId> {
        self.merged.ops.keys()
    }
}

impl Pending {
    pub(super) fn add(&self, notifications: &[Arc<DbNotification>], overflowed: bool) {
        {
            let mut changes = self.changes.lock().unwrap_or_else(PoisonError::into_inner);
            changes.all |= overflowed;
            for notification in notifications {
                for (entity, ops) in &notification.ops {
                    let merged = changes.merged.ops.entry(entity.clone()).or_default();
                    *merged |= *ops;
                }
            }
        }
        self.wake.notify_one();
    }

    pub(super) fn take(&self) -> Changes {
        std::mem::take(&mut *self.changes.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

#[cfg(test)]
mod tests {
    use aircoreclient::ChatId;
    use aircoreclient::db::notification::DbOperation;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn changes_merge_their_operations_and_the_overflow_flag() {
        let pending = Pending::default();
        let chat = DbEntityId::Chat(ChatId {
            uuid: Uuid::new_v4(),
        });
        let notification = |op: DbOperation| {
            let mut notification = DbNotification::default();
            notification.ops.insert(chat.clone(), op.into());
            Arc::new(notification)
        };
        pending.add(&[notification(DbOperation::Add)], true);
        pending.add(&[notification(DbOperation::Update)], false);

        let changes = pending.take();
        // An overflow isn't lost to a later batch, nor an earlier add to a
        // later update.
        assert!(changes.all);
        assert_eq!(
            changes.merged.ops[&chat],
            DbOperation::Add | DbOperation::Update
        );
        assert!(!pending.take().all);
    }
}
