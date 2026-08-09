-- Expose Application Desk through server-driven navigation.
--
-- Migration 0018 introduced the module and its grants before navigation became
-- data-driven in 0019. Add the missing section for every existing tenant; its
-- module-key rule makes it visible only to callers with application-desk.* access.
INSERT INTO platform.navigation_sections
    (tenant_id, section_key, kind, label, route, icon, sort_order,
     required_permissions, module_key, always_visible)
SELECT tenant.id, 'application-desk', 'workspace', 'Application Desk',
       '/dashboard/application-desk', 'IdCard', 45,
       ARRAY[]::text[], 'application-desk', false
FROM platform.tenants tenant
ON CONFLICT (tenant_id, section_key) DO NOTHING;
