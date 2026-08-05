use std::collections::HashMap;

use serde_json::Value;

use crate::models::EffectiveWidget;
use crate::state::EffectiveAccess;

pub const STAFF_DASHBOARD_NAMESPACE: &str = "dashboard.staff";

pub const DEFAULT_WIDGETS: &[(&str, Option<&str>)] = &[
    ("profile", None),
    ("avg_response_time", None),
    ("admission_velocity", None),
    ("counselor_sla", Some("dashboard.counselor_sla.read")),
    ("track_team", Some("dashboard.track_team.read")),
    ("talent_recruitment", None),
    ("pipeline_spread", Some("dashboard.pipeline_spread.read")),
    ("follow_ups", Some("dashboard.follow_ups.read")),
    ("fee_readiness", Some("dashboard.fee_readiness.read")),
    ("source_quality", Some("dashboard.source_quality.read")),
];

struct WidgetOverride {
    enabled: Option<bool>,
    required_permission: Option<Option<String>>,
}

fn parse_overrides(definition: Option<&Value>) -> HashMap<String, WidgetOverride> {
    let mut overrides = HashMap::new();
    let Some(widgets) = definition
        .and_then(|value| value.get("widgets"))
        .and_then(Value::as_array)
    else {
        return overrides;
    };
    for widget in widgets {
        let Some(id) = widget.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !DEFAULT_WIDGETS.iter().any(|(known, _)| known == &id) {
            continue;
        }
        let enabled = widget.get("enabled").and_then(Value::as_bool);
        let required_permission = match widget.get("requiredPermission") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => match value.as_str() {
                Some(permission) if !permission.trim().is_empty() => {
                    Some(Some(permission.to_owned()))
                }
                _ => None,
            },
        };
        overrides.insert(
            id.to_owned(),
            WidgetOverride {
                enabled,
                required_permission,
            },
        );
    }
    overrides
}

pub fn effective_widgets(
    definition: Option<&Value>,
    access: &EffectiveAccess,
) -> Vec<EffectiveWidget> {
    let overrides = parse_overrides(definition);
    DEFAULT_WIDGETS
        .iter()
        .filter_map(|(id, default_permission)| {
            let mut enabled = true;
            let mut required = default_permission.map(str::to_owned);
            if let Some(overrides) = overrides.get(*id) {
                if let Some(value) = overrides.enabled {
                    enabled = value;
                }
                if let Some(value) = &overrides.required_permission {
                    required = value.clone();
                }
            }
            let allowed = match &required {
                None => true,
                Some(permission) => access.allows(permission),
            };
            (enabled && allowed).then(|| EffectiveWidget {
                id: (*id).to_owned(),
                required_permission: required,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn access(permissions: &[&str]) -> EffectiveAccess {
        EffectiveAccess {
            roles: Vec::new(),
            permissions: permissions.iter().map(|permission| permission.to_string()).collect(),
            scopes: HashMap::new(),
        }
    }

    fn ids(widgets: &[EffectiveWidget]) -> Vec<&str> {
        widgets.iter().map(|widget| widget.id.as_str()).collect()
    }

    #[test]
    fn wildcard_sees_every_widget() {
        let widgets = effective_widgets(None, &access(&["*"]));
        assert_eq!(widgets.len(), DEFAULT_WIDGETS.len());
    }

    #[test]
    fn empty_permissions_see_only_ungated_widgets() {
        let widgets = effective_widgets(None, &access(&[]));
        assert_eq!(
            ids(&widgets),
            vec![
                "profile",
                "avg_response_time",
                "admission_velocity",
                "talent_recruitment"
            ]
        );
    }

    #[test]
    fn widget_grant_adds_only_that_widget() {
        let widgets = effective_widgets(None, &access(&["dashboard.pipeline_spread.read"]));
        assert_eq!(
            ids(&widgets),
            vec![
                "profile",
                "avg_response_time",
                "admission_velocity",
                "talent_recruitment",
                "pipeline_spread"
            ]
        );
    }

    #[test]
    fn override_disables_widget_for_everyone() {
        let definition = json!({
            "widgets": [
                { "id": "source_quality", "enabled": false }
            ]
        });
        let widgets = effective_widgets(Some(&definition), &access(&["*"]));
        assert!(!ids(&widgets).contains(&"source_quality"));
        assert_eq!(widgets.len(), DEFAULT_WIDGETS.len() - 1);
    }

    #[test]
    fn override_repoints_required_permission() {
        let definition = json!({
            "widgets": [
                { "id": "source_quality", "requiredPermission": "crm.reports.read" }
            ]
        });
        let widgets = effective_widgets(Some(&definition), &access(&["crm.reports.read"]));
        assert!(ids(&widgets).contains(&"source_quality"));

        let denied = effective_widgets(Some(&definition), &access(&[]));
        assert!(!ids(&denied).contains(&"source_quality"));
    }

    #[test]
    fn override_can_ungate_a_widget() {
        let definition = json!({
            "widgets": [
                { "id": "counselor_sla", "requiredPermission": null }
            ]
        });
        let widgets = effective_widgets(Some(&definition), &access(&[]));
        assert!(ids(&widgets).contains(&"counselor_sla"));
    }

    #[test]
    fn malformed_definition_falls_back_to_defaults() {
        let definition = json!({
            "widgets": [
                { "id": "unknown_widget", "enabled": false },
                { "enabled": false },
                "garbage",
                { "id": "track_team", "requiredPermission": 42 }
            ]
        });
        let widgets = effective_widgets(Some(&definition), &access(&["*"]));
        assert_eq!(widgets.len(), DEFAULT_WIDGETS.len());

        // "requiredPermission": 42 is malformed, so the override is ignored and
        // track_team keeps its default permission requirement.
        let widgets = effective_widgets(Some(&definition), &access(&[]));
        assert!(!ids(&widgets).contains(&"track_team"));
    }
}
