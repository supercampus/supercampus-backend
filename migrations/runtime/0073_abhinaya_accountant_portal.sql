-- Rename the MEC accountant identity while preserving its password and history,
-- and ensure the account can only enter the accountant finance portal.
DO $$
DECLARE
    mec_tenant_id uuid;
    accountant_role_id uuid;
    accountant_user_id uuid := '4b273ab2-0a54-571d-b53d-904fecb013d4'::uuid;
BEGIN
    SELECT id INTO mec_tenant_id FROM platform.tenants WHERE slug = 'mec';
    IF mec_tenant_id IS NULL THEN
        RETURN;
    END IF;

    UPDATE identity.users
       SET email = 'abhinaya@mec.local',
           display_name = 'Abhinaya',
           initials = 'A',
           account_type = 'staff',
           active = true,
           profile = '{"designation":"Accountant","team":"Finance","dept":""}'::jsonb,
           updated_at = now()
     WHERE id = accountant_user_id;

    UPDATE identity.tenant_memberships
       SET roles = ARRAY['accountant']::text[],
           active = true,
           is_primary = true,
           profile = '{"designation":"Accountant","team":"Finance","dept":""}'::jsonb,
           updated_at = now()
     WHERE tenant_id = mec_tenant_id AND user_id = accountant_user_id;

    SELECT id INTO accountant_role_id
      FROM authz.roles
     WHERE tenant_id = mec_tenant_id AND lower(role_key) = 'accountant'
     LIMIT 1;
    IF accountant_role_id IS NOT NULL THEN
        DELETE FROM authz.user_roles
         WHERE tenant_id = mec_tenant_id AND user_id = accountant_user_id
           AND role_id <> accountant_role_id;
        INSERT INTO authz.user_roles (tenant_id, user_id, role_id, assigned_by)
        VALUES (mec_tenant_id, accountant_user_id, accountant_role_id, 'migration-0073')
        ON CONFLICT (tenant_id, user_id, role_id) DO NOTHING;
    END IF;

    UPDATE core.employees
       SET full_name = 'Abhinaya',
           email = 'abhinaya@mec.local',
           status = 'active',
           profile = '{"designation":"Accountant","team":"Finance","dept":""}'::jsonb,
           updated_at = now()
     WHERE tenant_id = mec_tenant_id AND user_id = accountant_user_id;

    UPDATE identity.auth_sessions
       SET revoked_at = COALESCE(revoked_at, now())
     WHERE tenant_id = mec_tenant_id AND user_id = accountant_user_id::text;
END $$;
