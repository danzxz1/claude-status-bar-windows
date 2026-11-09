use crate::model::{Appearance, Provider};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct Notice {
    pub provider: String,
    pub title: String,
    pub text: String,
}
#[derive(Default)]
pub struct Notifications {
    states: HashMap<(String, String), String>,
    limits: HashMap<(String, String), (i64, f64)>,
    resets: HashSet<(String, String, i64)>,
    initialized: bool,
    thresholds: HashSet<(String, String, i64, u16)>,
}
impl Notifications {
    pub fn observe(
        &mut self,
        providers: &[Provider],
        settings: &Appearance,
        now: i64,
    ) -> Vec<Notice> {
        let mut notices = Vec::new();
        for provider in providers {
            for session in &provider.sessions {
                let key = (provider.name.clone(), session.session_id.clone());
                if let Some(old) = self.states.insert(key, session.state.clone()) {
                    let permission = session.state == "permission"
                        && old != "permission"
                        && settings.notify_permission;
                    let done = session.state == "done"
                        && matches!(old.as_str(), "thinking" | "tool" | "permission")
                        && settings.notify_done;
                    if permission || done {
                        notices.push(Notice {
                            provider: provider.name.clone(),
                            title: format!(
                                "{} · {}",
                                provider.name,
                                if permission {
                                    "Нужно разрешение"
                                } else {
                                    "Работа завершена"
                                }
                            ),
                            text: session.task().into(),
                        });
                    }
                }
            }
            if let Some(usage) = &provider.usage {
                for (name, window) in [("5 часов", &usage.five_hour), ("7 дней", &usage.seven_day)]
                {
                    let Some(window) = window else { continue };
                    let key = (provider.name.clone(), name.to_owned());
                    if !self.limits.contains_key(&key) {
                        for threshold in [80u16, 90, 100] {
                            if window.used >= threshold as f64 {
                                self.thresholds.insert((
                                    key.0.clone(),
                                    key.1.clone(),
                                    window.resets_at,
                                    threshold,
                                ));
                            }
                        }
                    }
                    if !self.limits.contains_key(&key) && window.resets_at <= now {
                        self.resets
                            .insert((key.0.clone(), key.1.clone(), window.resets_at));
                    }
                    if let Some((old_reset, old_used)) = self
                        .limits
                        .insert(key.clone(), (window.resets_at, window.used))
                    {
                        let mut threshold_notice = None;
                        for threshold in [80u16, 90, 100] {
                            if window.used >= threshold as f64
                                && self.thresholds.insert((
                                    key.0.clone(),
                                    key.1.clone(),
                                    window.resets_at,
                                    threshold,
                                ))
                                && old_reset == window.resets_at
                                && old_used < threshold as f64
                            {
                                threshold_notice = Some(threshold);
                            }
                        }
                        if settings.notify_limits && window.resets_at > now {
                            if let Some(threshold) = threshold_notice {
                                notices.push(Notice{provider:provider.name.clone(),title:format!("{} · Лимит {}",provider.name,name),text:format!("Использовано {threshold}% лимита. Текущее значение: {:.0}%.",window.used)});
                            }
                        }
                        if settings.notify_reset
                            && old_reset <= now
                            && self
                                .resets
                                .insert((key.0.clone(), key.1.clone(), old_reset))
                        {
                            notices.push(Notice { provider: provider.name.clone(), title: format!("{} · Сброс лимита", provider.name),
                                text: format!("Окно «{name}» завершилось. Актуальные проценты появятся после обновления данных."), });
                        }
                    }
                }
            }
        }
        // Уведомляем о локальном истечении окна, даже если новое событие его уже не содержит.
        for ((provider, name), (reset, _)) in &self.limits {
            if *reset <= now
                && self.resets.insert((provider.clone(), name.clone(), *reset))
                && settings.notify_reset
            {
                notices.push(Notice {
                    provider: provider.clone(),
                    title: format!("{provider} · Сброс лимита"),
                    text: format!("Окно «{name}» завершилось. Ожидаем актуальные проценты."),
                });
            }
        }
        // Удаление сессии из снимка не означает завершение ответа. Историю не накапливаем бесконечно.
        self.states.retain(|(p, id), _| {
            providers
                .iter()
                .any(|v| &v.name == p && v.sessions.iter().any(|s| &s.session_id == id))
        });
        if self.resets.len() > 64 {
            self.resets.retain(|(_, _, reset)| now - *reset < 7 * 86400);
        }
        if self.thresholds.len() > 128 {
            self.thresholds
                .retain(|(_, _, reset, _)| now - *reset < 7 * 86400);
        }
        let initialized = self.initialized;
        self.initialized = true;
        if !initialized || !settings.notifications || settings.quiet {
            notices.clear();
        }
        notices
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use chrono::Utc;
    fn provider(state: &str, used: f64, reset: i64) -> Provider {
        Provider {
            name: "Codex".into(),
            sessions: vec![Session {
                session_id: "one".into(),
                state: state.into(),
                project: "Тестовая задача".into(),
                ..Default::default()
            }],
            usage: Some(Usage {
                five_hour: Some(Limit {
                    used,
                    resets_at: reset,
                }),
                seven_day: None,
                updated_at: Utc::now(),
            }),
            notice: None,
        }
    }
    #[test]
    fn no_old_notifications_and_state_transitions() {
        let mut n = Notifications::default();
        let a = Appearance::default();
        assert!(
            n.observe(&[provider("thinking", 81., 200)], &a, 100)
                .is_empty()
        );
        assert_eq!(
            n.observe(&[provider("permission", 81., 200)], &a, 100)
                .len(),
            1
        );
        assert!(
            n.observe(&[provider("permission", 81., 200)], &a, 100)
                .is_empty()
        );
        assert_eq!(n.observe(&[provider("done", 81., 200)], &a, 100).len(), 1);
    }
    #[test]
    fn expired_history_is_silent_and_missing_window_can_reset() {
        let mut n = Notifications::default();
        let a = Appearance::default();
        assert!(n.observe(&[provider("done", 91., 100)], &a, 200).is_empty());
        assert!(n.observe(&[provider("done", 91., 100)], &a, 201).is_empty());
        let mut n = Notifications::default();
        n.observe(&[provider("done", 20., 300)], &a, 200);
        let mut missing = provider("done", 20., 300);
        missing.usage = None;
        assert_eq!(n.observe(&[missing], &a, 301).len(), 1);
    }
    #[test]
    fn thresholds_are_not_repeated_after_usage_fluctuates() {
        let mut n = Notifications::default();
        let a = Appearance::default();
        n.observe(&[provider("thinking", 70., 300)], &a, 100);
        assert_eq!(
            n.observe(&[provider("thinking", 91., 300)], &a, 101).len(),
            1
        );
        assert!(
            n.observe(&[provider("thinking", 60., 300)], &a, 102)
                .is_empty()
        );
        assert!(
            n.observe(&[provider("thinking", 91., 300)], &a, 103)
                .is_empty()
        );
    }
    #[test]
    fn threshold_reset_and_quiet_mode() {
        let mut n = Notifications::default();
        let a = Appearance::default();
        n.observe(&[provider("thinking", 70., 200)], &a, 100);
        assert_eq!(
            n.observe(&[provider("thinking", 91., 200)], &a, 100).len(),
            1
        );
        assert!(
            n.observe(&[provider("thinking", 91., 200)], &a, 100)
                .is_empty()
        );
        assert_eq!(
            n.observe(&[provider("thinking", 91., 200)], &a, 201).len(),
            1
        );
        assert!(
            n.observe(&[provider("thinking", 91., 200)], &a, 202)
                .is_empty()
        );
        let quiet = Appearance {
            quiet: true,
            ..a.clone()
        };
        assert!(
            n.observe(&[provider("done", 100., 200)], &quiet, 203)
                .is_empty()
        );
        assert!(
            n.observe(&[provider("done", 100., 200)], &a, 204)
                .is_empty()
        );
    }
}
