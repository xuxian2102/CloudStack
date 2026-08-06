//! 设置写盘的纯判定逻辑：单写者 + 最新值合并，不依赖 GTK/glib/gio/adw。
//! GTK 侧的 thread_local 运行时（`window/settings.rs`）负责实际派发
//! `tasks::run` 和把结果送回这里的 [`SettingsWriter::complete_write`]。

use cloudstack_core::services::settings::AppSettings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedSettings {
    pub generation: u64,
    pub snapshot: AppSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsWriterAction {
    None,
    Persist(VersionedSettings),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsWriteTransition {
    pub next_write: Option<VersionedSettings>,
    pub report_failure: bool,
}

/// 内存里的权威设置快照 + 单写者协调。任意时刻最多一个写盘任务在飞；写入
/// 进行中又来了新的修改，只保留最新快照到 `pending`，跳过的中间快照不落盘。
pub struct SettingsWriter {
    current: AppSettings,
    next_generation: u64,
    in_flight: Option<VersionedSettings>,
    pending: Option<VersionedSettings>,
    /// 上一次写盘失败、且之后没有更新的快照顶替它时保留在这里，避免那次
    /// 修改被直接丢弃。下一次修改（`update`）或显式重试（`retry_failed_write`）
    /// 会把它重新排进写盘队列。
    retry_pending: Option<VersionedSettings>,
}

impl SettingsWriter {
    pub fn new(current: AppSettings) -> Self {
        Self {
            current,
            next_generation: 0,
            in_flight: None,
            pending: None,
            retry_pending: None,
        }
    }

    pub fn current(&self) -> &AppSettings {
        &self.current
    }

    /// 立即修改内存中的完整设置，再决定要不要触发一次写盘。新的修改总是
    /// 比任何待重试的失败快照更权威，所以会顶掉 `retry_pending`。
    pub fn update(&mut self, edit: impl FnOnce(&mut AppSettings)) -> SettingsWriterAction {
        edit(&mut self.current);
        self.retry_pending = None;
        let snapshot = self.current.clone();
        self.enqueue(snapshot)
    }

    /// 把上一次失败、目前没有更新快照顶替的快照重新排进写盘队列。没有可重
    /// 试的快照，或者恰好已经有写入在飞（正常不会同时发生），什么都不做。
    pub fn retry_failed_write(&mut self) -> SettingsWriterAction {
        let Some(retry) = self.retry_pending.take() else {
            return SettingsWriterAction::None;
        };
        self.enqueue(retry.snapshot)
    }

    fn enqueue(&mut self, snapshot: AppSettings) -> SettingsWriterAction {
        let versioned = VersionedSettings {
            generation: self.next_generation,
            snapshot,
        };
        self.next_generation += 1;
        if self.in_flight.is_some() {
            self.pending = Some(versioned);
            SettingsWriterAction::None
        } else {
            self.in_flight = Some(versioned.clone());
            SettingsWriterAction::Persist(versioned)
        }
    }

    /// 写盘任务完成时调用一次。`generation` 只用于防御性校验——这个设计下
    /// 任意时刻最多一个写入在飞，正常情况下它必然等于当前 `in_flight` 的
    /// generation。
    pub fn complete_write(&mut self, generation: u64, success: bool) -> SettingsWriteTransition {
        let Some(in_flight) = self.in_flight.take() else {
            return SettingsWriteTransition::default();
        };
        debug_assert_eq!(
            in_flight.generation, generation,
            "任意时刻最多一个写入在飞，completion 的 generation 应该总是匹配"
        );

        if let Some(pending) = self.pending.take() {
            self.in_flight = Some(pending.clone());
            return SettingsWriteTransition {
                next_write: Some(pending),
                report_failure: !success,
            };
        }

        if !success {
            self.retry_pending = Some(in_flight);
        }
        SettingsWriteTransition {
            next_write: None,
            report_failure: !success,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_snapshot(action: SettingsWriterAction) -> AppSettings {
        match action {
            SettingsWriterAction::Persist(versioned) => versioned.snapshot,
            SettingsWriterAction::None => panic!("expected a Persist action"),
        }
    }

    #[test]
    fn first_update_starts_a_write() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        let action = writer.update(|settings| settings.auto_reopen_last_project = true);
        let snapshot = action_snapshot(action);
        assert!(snapshot.auto_reopen_last_project);
    }

    #[test]
    fn second_update_while_writing_becomes_pending() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        writer.update(|settings| settings.auto_reopen_last_project = true);
        let action = writer.update(|settings| settings.restore_last_document_on_open = true);
        assert_eq!(action, SettingsWriterAction::None);
    }

    #[test]
    fn multiple_pending_updates_keep_only_the_latest() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        let first = writer.update(|settings| settings.auto_reopen_last_project = true);
        writer.update(|settings| settings.restore_last_document_on_open = true);
        writer.update(|settings| settings.restore_last_document_on_open = false);

        let started_generation = match first {
            SettingsWriterAction::Persist(versioned) => versioned.generation,
            SettingsWriterAction::None => panic!("first update should start a write"),
        };
        let transition = writer.complete_write(started_generation, true);
        let next = transition.next_write.expect("a pending write should start");
        // 中间那次"改成 true"被跳过，只落最后一次"改成 false"。
        assert!(!next.snapshot.restore_last_document_on_open);
        assert!(next.snapshot.auto_reopen_last_project);
    }

    #[test]
    fn completion_starts_the_latest_pending_write() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        let first = writer.update(|settings| settings.auto_reopen_last_project = true);
        let started_generation = match first {
            SettingsWriterAction::Persist(versioned) => versioned.generation,
            SettingsWriterAction::None => unreachable!(),
        };
        writer.update(|settings| settings.restore_last_document_on_open = true);

        let transition = writer.complete_write(started_generation, true);
        assert!(!transition.report_failure);
        assert!(transition.next_write.is_some());
    }

    #[test]
    fn single_field_update_preserves_other_settings() {
        let mut writer = SettingsWriter::new(AppSettings {
            auto_reopen_last_project: true,
            restore_last_document_on_open: true,
            ..Default::default()
        });
        let action = writer.update(|settings| {
            settings.color_scheme = cloudstack_core::services::settings::ColorScheme::Dark
        });
        let snapshot = action_snapshot(action);
        assert!(snapshot.auto_reopen_last_project);
        assert!(snapshot.restore_last_document_on_open);
    }

    #[test]
    fn failed_write_does_not_drop_a_newer_pending_snapshot() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        let first = writer.update(|settings| settings.auto_reopen_last_project = true);
        let started_generation = match first {
            SettingsWriterAction::Persist(versioned) => versioned.generation,
            SettingsWriterAction::None => unreachable!(),
        };
        writer.update(|settings| settings.restore_last_document_on_open = true);

        let transition = writer.complete_write(started_generation, false);
        assert!(transition.report_failure);
        let next = transition
            .next_write
            .expect("newer pending snapshot must still be written");
        assert!(next.snapshot.restore_last_document_on_open);
    }

    #[test]
    fn failed_write_with_no_pending_keeps_a_retryable_snapshot() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        let first = writer.update(|settings| settings.auto_reopen_last_project = true);
        let started_generation = match first {
            SettingsWriterAction::Persist(versioned) => versioned.generation,
            SettingsWriterAction::None => unreachable!(),
        };

        let transition = writer.complete_write(started_generation, false);
        assert!(transition.report_failure);
        assert!(transition.next_write.is_none());

        let retry_action = writer.retry_failed_write();
        let snapshot = action_snapshot(retry_action);
        assert!(snapshot.auto_reopen_last_project);
    }

    #[test]
    fn next_update_after_failure_supersedes_the_retryable_snapshot() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        let first = writer.update(|settings| settings.auto_reopen_last_project = true);
        let started_generation = match first {
            SettingsWriterAction::Persist(versioned) => versioned.generation,
            SettingsWriterAction::None => unreachable!(),
        };
        writer.complete_write(started_generation, false);

        let action = writer.update(|settings| settings.restore_last_document_on_open = true);
        let snapshot = action_snapshot(action);
        assert!(snapshot.auto_reopen_last_project);
        assert!(snapshot.restore_last_document_on_open);

        assert_eq!(writer.retry_failed_write(), SettingsWriterAction::None);
    }

    #[test]
    fn retry_failed_write_without_a_failure_does_nothing() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        assert_eq!(writer.retry_failed_write(), SettingsWriterAction::None);
    }

    #[test]
    fn completion_with_no_in_flight_write_is_a_noop() {
        let mut writer = SettingsWriter::new(AppSettings::default());
        assert_eq!(
            writer.complete_write(0, true),
            SettingsWriteTransition::default()
        );
    }
}
