-- Tenant-local library workflow plus MEC stationery catalogue and assignment.

CREATE TABLE IF NOT EXISTS campus_ops.library_visit_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES platform.tenants(id) ON DELETE CASCADE,
    student_user_id text NOT NULL,
    student_name text NOT NULL,
    zone_name text NOT NULL DEFAULT 'Central Library - Reading Hall',
    description text,
    visit_start timestamptz NOT NULL,
    visit_end timestamptz NOT NULL,
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','approved','rejected','cancelled','completed')),
    qr_token_hash text NOT NULL UNIQUE,
    qr_payload text NOT NULL UNIQUE,
    decision_note text,
    decided_by text,
    decided_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (visit_end > visit_start)
);
CREATE INDEX IF NOT EXISTS library_visit_requests_queue_idx
    ON campus_ops.library_visit_requests (tenant_id, status, visit_start);
CREATE INDEX IF NOT EXISTS library_visit_requests_student_idx
    ON campus_ops.library_visit_requests (tenant_id, student_user_id, created_at DESC);

DO $$
DECLARE
    mec_tenant uuid;
    stationery_shop uuid;
BEGIN
    SELECT id INTO mec_tenant FROM platform.tenants WHERE slug = 'mec';
    IF mec_tenant IS NULL THEN RETURN; END IF;

    INSERT INTO campus_ops.shops
        (tenant_id, shop_key, name, category, description, is_active, created_by)
    VALUES
        (mec_tenant, 'stationery', 'Stationery Store', 'Stationery',
         'Campus stationery and daily essentials', true, 'runtime-migration-0080')
    ON CONFLICT (tenant_id, shop_key) DO UPDATE SET
        name = EXCLUDED.name, category = EXCLUDED.category,
        description = EXCLUDED.description, is_active = true, updated_at = now()
    RETURNING id INTO stationery_shop;

END $$;

ALTER TABLE campus_ops.canteen_menu_items
    DROP CONSTRAINT IF EXISTS canteen_menu_items_tenant_id_name_key;
CREATE UNIQUE INDEX IF NOT EXISTS canteen_menu_items_store_name_price_unique
    ON campus_ops.canteen_menu_items (tenant_id, store, name, price);

WITH mec AS (
    SELECT id AS tenant_id FROM platform.tenants WHERE slug = 'mec'
), inventory(category, name, price) AS (
    VALUES
    ('Hair Care & Shampoo','Arial',10::numeric),('Hair Care & Shampoo','Clinic plus',1),('Hair Care & Shampoo','Chik',1),('Hair Care & Shampoo','Sunsilk',1),('Hair Care & Shampoo','Dove shampoo',5),('Hair Care & Shampoo','Dove shampoo and Conditioner (b)',660),('Hair Care & Shampoo','Loreal shampoo',90),('Hair Care & Shampoo','Head and shoulder',4),
    ('Soaps & Detergents','Surf excel soap big',34),('Soaps & Detergents','Surf excel soap small',10),('Soaps & Detergents','Surf excel liquid',10),('Soaps & Detergents','Rin 1 liter',99),('Soaps & Detergents','Ghar soap',185),('Soaps & Detergents','Mysore sandal',80),('Soaps & Detergents','Baby soap',70),('Soaps & Detergents','Pears Green',63),('Soaps & Detergents','Pears Yellow',58),('Soaps & Detergents','Medimix',35),('Soaps & Detergents','Himalaya honey soap',64),('Soaps & Detergents','Himalaya almond soap',52),('Soaps & Detergents','Hamam',40),('Soaps & Detergents','Nature power',40),('Soaps & Detergents','Dove blue',80),('Soaps & Detergents','Dove pink',75),('Soaps & Detergents','Dove Green',68),('Soaps & Detergents','Cinthol Black',55),('Soaps & Detergents','Cinthol original',50),('Soaps & Detergents','Lux',40),('Soaps & Detergents','Lifebuoy',42),
    ('Bath Items','Bucket',160),('Bath Items','Mug',30),('Bath Items','Bath brush',65),('Bath Items','Cloth brush',55),('Bath Items','Cloth rope',100),('Bath Items','Broom stick',120),
    ('Health & Medical','Ponds',75),('Health & Medical','Iodex',15),('Health & Medical','Dettol',42),('Health & Medical','ENO',10),('Health & Medical','Sanitizer',35),
    ('Personal Care - Feminine','Stayfree Regular',37),('Personal Care - Feminine','Stayfree',45),('Personal Care - Feminine','Whisper',50),
    ('Oral Care','Toothbrush',20),('Oral Care','Toothbrush',30),('Oral Care','Toothbrush',40),('Oral Care','Toothbrush',65),('Oral Care','Sensodyne',95),('Oral Care','Close up',20),('Oral Care','Colgate',20),('Oral Care','Colgate',10),('Oral Care','Himalaya paste',20),('Oral Care','Dabur red',20),('Oral Care','Dabur red',10),
    ('Hair & Oil Care','Ear buds',10),('Hair & Oil Care','Coconut oil',20),('Hair & Oil Care','Navaratna oil',47),('Hair & Oil Care','Razor',25),('Hair & Oil Care','Comb',5),('Hair & Oil Care','Comb',10),('Hair & Oil Care','Shaving cream',20),
    ('Miscellaneous Items','Soap box',30),('Miscellaneous Items','Lock',70),('Miscellaneous Items','Safety pin',20),('Miscellaneous Items','Hook',15),('Miscellaneous Items','Boys kerchief',20),('Miscellaneous Items','Girls Kerchief',15),('Miscellaneous Items','Towel',30),('Miscellaneous Items','Rin ala',90),('Miscellaneous Items','Kosu vathi',15),('Miscellaneous Items','Comfort',4),('Miscellaneous Items','WB Marker INK Blue',30),('Miscellaneous Items','WB Marker INK Black',30),('Miscellaneous Items','Fewikwick',5),
    ('Stationery - Writing Instruments','Blue INK bottle',30),('Stationery - Writing Instruments','Catridge',4),('Stationery - Writing Instruments','Trimax refill',25),('Stationery - Writing Instruments','0.7 lead',5),('Stationery - Writing Instruments','0.5 lead',5),('Stationery - Writing Instruments','Pencil',6),('Stationery - Writing Instruments','Blue gel',10),('Stationery - Writing Instruments','Permanent marker',20),('Stationery - Writing Instruments','White board marker blue',27),('Stationery - Writing Instruments','White board marker black',27),('Stationery - Writing Instruments','5rs pen',5),('Stationery - Writing Instruments','Blue pen',10),('Stationery - Writing Instruments','Black pen',10),('Stationery - Writing Instruments','Red pen',10),('Stationery - Writing Instruments','Matrix Ink Pen',60),
    ('Stationery - Office Supplies','Cello tape',7),('Stationery - Office Supplies','Scissors',40),('Stationery - Office Supplies','Fevicol',10),('Stationery - Office Supplies','Fevigum',5),('Stationery - Office Supplies','Fevistik',15),('Stationery - Office Supplies','Paper pin',35),('Stationery - Office Supplies','Rubber band',15),('Stationery - Office Supplies','Mechanical pencil 0.5',10),('Stationery - Office Supplies','Mechanical pencil 0.7',10),('Stationery - Office Supplies','Mechanical pencil 2.0',30),('Stationery - Office Supplies','Staples',12),('Stationery - Office Supplies','Stapler',75),('Stationery - Office Supplies','Highlighter',25),('Stationery - Office Supplies','Scale',12),('Stationery - Office Supplies','Small scale',5),('Stationery - Office Supplies','Apsara sharpner',5),('Stationery - Office Supplies','Eraser',5),('Stationery - Office Supplies','Whitener',30),
    ('Stationery - Paper & Files','White chart',10),('Stationery - Paper & Files','Button file',25),('Stationery - Paper & Files','Transparent file',15),('Stationery - Paper & Files','Stick file',10),('Stationery - Paper & Files','60pg unruled',30),('Stationery - Paper & Files','92pg ruled',40),('Stationery - Paper & Files','92pg unruled',40),('Stationery - Paper & Files','120pg ruled',50),('Stationery - Paper & Files','120pg unruled',50),('Stationery - Paper & Files','160pg unruled',60),('Stationery - Paper & Files','240pg ruled',100),('Stationery - Paper & Files','240pg unruled',100),('Stationery - Paper & Files','320pg unruled',150),('Stationery - Paper & Files','Graph',1.5),('Stationery - Paper & Files','Exam paper',2),('Stationery - Paper & Files','Pouch',20),
    ('Electronics & Batteries','AA battery',18),('Electronics & Batteries','AAA battery',18)
)
INSERT INTO campus_ops.canteen_menu_items
    (tenant_id, name, description, store, category, price, actual_price, prep_minutes,
     is_vegetarian, is_popular, is_available, is_instant, created_by)
SELECT mec.tenant_id, inventory.name, inventory.category, 'stationery',
       inventory.category, inventory.price, inventory.price, 1, true, false, true, true,
       'runtime-migration-0080'
FROM mec CROSS JOIN inventory
ON CONFLICT (tenant_id, store, name, price) DO UPDATE SET
    description = EXCLUDED.description, category = EXCLUDED.category,
    prep_minutes = 1, is_available = true, is_instant = true,
    updated_at = now();
