-- Six-digit gate codes provide enough space for every active campus pass.
WITH numbered AS (
    SELECT id,
           100000 + row_number() OVER (PARTITION BY tenant_id ORDER BY id) - 1 AS code
    FROM campus_ops.gatepass_requests
    WHERE state = 'approved'
      AND (manual_code IS NULL OR manual_code !~ '^[0-9]{6}$')
)
UPDATE campus_ops.gatepass_requests request
SET manual_code = numbered.code::text,
    manual_code_hash = encode(digest(numbered.code::text, 'sha256'), 'hex')
FROM numbered
WHERE numbered.id = request.id
  AND numbered.code <= 499999;

WITH numbered AS (
    SELECT id,
           500000 + row_number() OVER (PARTITION BY tenant_id ORDER BY id) - 1 AS code
    FROM campus_ops.daily_access_passes
    WHERE manual_code IS NULL OR manual_code !~ '^[0-9]{6}$'
)
UPDATE campus_ops.daily_access_passes pass
SET manual_code = numbered.code::text,
    manual_code_hash = encode(digest(numbered.code::text, 'sha256'), 'hex')
FROM numbered
WHERE numbered.id = pass.id
  AND numbered.code <= 999999;
