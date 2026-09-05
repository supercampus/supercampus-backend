CREATE TABLE IF NOT EXISTS campus_ops.laundry_settings (
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    shop_key text NOT NULL DEFAULT 'mec-laundry',
    price_per_kg numeric(12,2) NOT NULL DEFAULT 0 CHECK (price_per_kg >= 0),
    updated_by text,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, shop_key)
);

CREATE TABLE IF NOT EXISTS campus_ops.laundry_charges (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    shop_key text NOT NULL DEFAULT 'mec-laundry',
    service_type text NOT NULL CHECK (service_type IN ('wash','ironing')),
    name text NOT NULL,
    description text NOT NULL DEFAULT '',
    quantity numeric(10,2) NOT NULL CHECK (quantity > 0),
    unit_label text NOT NULL CHECK (unit_label IN ('kg','clothes')),
    unit_price numeric(12,2),
    total numeric(12,2) NOT NULL CHECK (total > 0),
    qr_token_hash text NOT NULL UNIQUE,
    created_by text NOT NULL,
    claimed_by text,
    claimed_at timestamptz,
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','claimed','paid','cancelled')),
    paid_at timestamptz,
    wallet_transaction_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS laundry_charges_student_idx
    ON campus_ops.laundry_charges (tenant_id, claimed_by, status, created_at DESC);
CREATE INDEX IF NOT EXISTS laundry_charges_shop_idx
    ON campus_ops.laundry_charges (tenant_id, shop_key, created_at DESC);

DO $$
DECLARE
    mec_tenant uuid;
    laundry_shop uuid;
    laundry_user uuid;
    owner_role uuid;
    source_password text;
BEGIN
    SELECT id INTO mec_tenant FROM platform.tenants WHERE slug='mec';
    IF mec_tenant IS NULL THEN RETURN; END IF;

    INSERT INTO campus_ops.shops
        (tenant_id,shop_key,name,category,description,is_active,created_by)
    VALUES
        (mec_tenant,'mec-laundry','Campus Laundry','laundry',
         'QR-based washing and ironing payments',true,'runtime-migration-0090')
    ON CONFLICT(tenant_id,shop_key) DO UPDATE SET
        name='Campus Laundry',category='laundry',is_active=true,updated_at=now()
    RETURNING id INTO laundry_shop;

    INSERT INTO campus_ops.laundry_settings(tenant_id,shop_key)
    VALUES(mec_tenant,'mec-laundry') ON CONFLICT DO NOTHING;

    SELECT password_hash INTO source_password FROM identity.users
      WHERE email='laundry.owner@mec.local';
    IF source_password IS NULL THEN
        SELECT password_hash INTO source_password FROM identity.users
          WHERE email='principal@mec.local';
    END IF;
    IF source_password IS NULL THEN
        source_password := crypt('Mec@2026', gen_salt('bf',12));
    END IF;

    INSERT INTO identity.users
        (email,password_hash,display_name,initials,account_type,active,profile)
    VALUES
        ('laundry@mec.local',source_password,'MEC Laundry','ML','staff',true,
         '{"shop":"Campus Laundry","post":"owner","team":"Vendors"}'::jsonb)
    ON CONFLICT(email) DO UPDATE SET
        display_name='MEC Laundry',initials='ML',account_type='staff',active=true,
        profile=identity.users.profile || EXCLUDED.profile,updated_at=now()
    RETURNING id INTO laundry_user;

    SELECT id INTO owner_role FROM authz.roles
      WHERE tenant_id=mec_tenant AND role_key='owner' AND active LIMIT 1;
    IF owner_role IS NULL THEN
        RAISE EXCEPTION 'MEC owner role is unavailable';
    END IF;

    UPDATE identity.tenant_memberships SET is_primary=false,updated_at=now()
      WHERE user_id=laundry_user AND is_primary AND tenant_id<>mec_tenant;
    INSERT INTO identity.tenant_memberships
        (tenant_id,user_id,roles,active,is_primary,profile)
    VALUES
        (mec_tenant,laundry_user,ARRAY['owner']::text[],true,true,
         '{"shop":"Campus Laundry","post":"owner","team":"Vendors"}'::jsonb)
    ON CONFLICT(tenant_id,user_id) DO UPDATE SET
        roles=EXCLUDED.roles,active=true,is_primary=true,
        profile=identity.tenant_memberships.profile || EXCLUDED.profile,
        updated_at=now();

    DELETE FROM authz.user_roles WHERE tenant_id=mec_tenant AND user_id=laundry_user;
    INSERT INTO authz.user_roles(tenant_id,user_id,role_id,assigned_by)
    VALUES(mec_tenant,laundry_user,owner_role,'runtime-migration-0090')
    ON CONFLICT DO NOTHING;

    INSERT INTO campus_ops.shop_user_assignments
        (tenant_id,shop_id,user_id,assignment_role,is_active,assigned_by)
    VALUES
        (mec_tenant,laundry_shop,laundry_user::text,'owner',true,'runtime-migration-0090')
    ON CONFLICT(tenant_id,shop_id,user_id) DO UPDATE SET
        assignment_role='owner',is_active=true,assigned_by=EXCLUDED.assigned_by,
        updated_at=now();

    INSERT INTO campus_ops.canteen_staff_state(tenant_id,user_id,mode,shop_open)
    VALUES(mec_tenant,laundry_user::text,'work',true)
    ON CONFLICT(tenant_id,user_id) DO UPDATE SET mode='work',updated_at=now();
END $$;
