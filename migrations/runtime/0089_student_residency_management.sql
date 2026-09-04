INSERT INTO authz.permission_templates
    (permission_key,module_key,feature_key,action,crud_actions,display_name,description,active)
VALUES
    ('students.directory.update','students','directory','update',ARRAY['update']::text[],
     'Update student profiles','Change managed Student Master profile fields including residency',true)
ON CONFLICT (permission_key) DO UPDATE SET active=true,updated_at=now();

INSERT INTO authz.permission_definitions
    (tenant_id,permission_key,module_key,feature_key,action,crud_actions,display_name,description,active)
SELECT tenant.id,template.permission_key,template.module_key,template.feature_key,template.action,
       template.crud_actions,template.display_name,template.description,true
FROM platform.tenants tenant
JOIN authz.permission_templates template ON template.permission_key='students.directory.update'
ON CONFLICT (tenant_id,permission_key) DO UPDATE SET active=true,updated_at=now();

INSERT INTO authz.role_permissions
    (tenant_id,role_id,permission_key,surface,scope,constraints,granted_by,granted_at)
SELECT role.tenant_id,role.id,'students.directory.update',surface.name,'institution','{}'::jsonb,
       'runtime-migration-0089',now()
FROM authz.roles role
CROSS JOIN (VALUES ('app'::text),('website'::text)) surface(name)
WHERE role.role_key IN ('tenant_admin','admin','administrator','super_admin') AND role.active
ON CONFLICT (tenant_id,role_id,surface,permission_key) DO UPDATE SET
    scope='institution',granted_by='runtime-migration-0089',granted_at=now();
