-- Four-digit fallback codes for gate staff when a camera cannot read the QR.
ALTER TABLE campus_ops.gatepass_requests
    ADD COLUMN IF NOT EXISTS manual_code text,
    ADD COLUMN IF NOT EXISTS manual_code_hash text;

ALTER TABLE campus_ops.daily_access_passes
    ADD COLUMN IF NOT EXISTS manual_code text,
    ADD COLUMN IF NOT EXISTS manual_code_hash text;

-- Existing approved passes receive a stable four-digit code immediately.
WITH numbered AS (
    SELECT id, 1000 + row_number() OVER (PARTITION BY tenant_id ORDER BY id) - 1 AS code
    FROM campus_ops.gatepass_requests
    WHERE state='approved' AND manual_code IS NULL
)
UPDATE campus_ops.gatepass_requests request
SET manual_code=numbered.code::text,
    manual_code_hash=encode(digest(numbered.code::text,'sha256'),'hex')
FROM numbered WHERE numbered.id=request.id AND numbered.code <= 4999;

WITH numbered AS (
    SELECT id, 5000 + row_number() OVER (PARTITION BY tenant_id ORDER BY id) - 1 AS code
    FROM campus_ops.daily_access_passes
    WHERE valid_on=CURRENT_DATE AND manual_code IS NULL
)
UPDATE campus_ops.daily_access_passes pass
SET manual_code=numbered.code::text,
    manual_code_hash=encode(digest(numbered.code::text,'sha256'),'hex')
FROM numbered WHERE numbered.id=pass.id AND numbered.code <= 9999;

CREATE UNIQUE INDEX IF NOT EXISTS gatepass_requests_manual_code_idx
    ON campus_ops.gatepass_requests(tenant_id,manual_code_hash)
    WHERE manual_code_hash IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS daily_access_manual_code_idx
    ON campus_ops.daily_access_passes(tenant_id,manual_code_hash)
    WHERE manual_code_hash IS NOT NULL;
