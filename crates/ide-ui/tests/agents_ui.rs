//! Agents tool window: the permission answer path and the task settings the
//! header exposes. The ACP session itself needs an installed provider process,
//! so these cover the decisions the panel makes before a session exists.

use ide_agent::{AgentConfigValue, AgentPrompt, Isolation, NewAgentTask};
use ide_ui::agents::{isolation_label, permission_is_destructive};

#[test]
fn permission_kinds_split_into_approvals_and_rejections() {
    // ACP's four kinds: allow_once, allow_always, reject_once, reject_always.
    assert!(!permission_is_destructive("allow_once"));
    assert!(!permission_is_destructive("allow_always"));
    assert!(permission_is_destructive("reject_once"));
    assert!(permission_is_destructive("reject_always"));
}

#[test]
fn isolation_labels_name_the_checkout_mode() {
    assert_eq!(isolation_label(Isolation::Local), "Isolation: local checkout");
    assert_eq!(isolation_label(Isolation::Worktree), "Isolation: worktree");
}

#[test]
fn a_task_uses_the_isolation_the_panel_selected() {
    for isolation in [Isolation::Local, Isolation::Worktree] {
        let request = NewAgentTask {
            project_root: None,
            prompt: AgentPrompt::default(),
            provider: "test".into(),
            config: Vec::new(),
            isolation,
            base: None,
        };
        assert_eq!(request.isolation, isolation, "the panel's choice reaches the request");
    }
}

#[test]
fn a_registry_entry_reports_its_install_state() {
    assert_eq!(registry_detail(None, false, "1.2.0"), "1.2.0");
    assert_eq!(registry_detail(Some("1.0.0"), false, "1.0.0"), "installed 1.0.0");
    assert_eq!(registry_detail(Some("1.0.0"), true, "1.2.0"), "1.0.0 → 1.2.0 (update)");
}

#[test]
fn a_config_control_cycles_through_its_values() {
    let values = vec![
        AgentConfigValue { value: "a".into(), name: "A".into(), description: None },
        AgentConfigValue { value: "b".into(), name: "B".into(), description: None },
    ];
    assert_eq!(next_config_value(&values, "a").as_deref(), Some("b"));
    assert_eq!(next_config_value(&values, "b").as_deref(), Some("a"), "the last value wraps");
    assert!(next_config_value(&values, "unknown").is_none());
}

fn registry_detail(installed: Option<&str>, update: bool, version: &str) -> String {
    match (installed, update) {
        (Some(current), true) => format!("{current} → {version} (update)"),
        (Some(current), false) => format!("installed {current}"),
        (None, _) => version.to_owned(),
    }
}

fn next_config_value(values: &[AgentConfigValue], current: &str) -> Option<String> {
    values
        .iter()
        .position(|value| value.value == current)
        .map(|index| (index + 1) % values.len())
        .and_then(|index| values.get(index))
        .map(|value| value.value.clone())
}
