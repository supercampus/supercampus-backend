-- Accountant wallet deductions.
--
-- An accountant can take money out of a store wallet (a correction, a fine, a
-- charge made outside the app). Unlike a purchase, a deduction may take the
-- wallet below zero, so the balance floor moves from the table into the
-- purchase paths: placing an order and paying a laundry charge still refuse
-- when the balance does not cover the total. Deductions are ledgered as
-- 'manual_debit' rows whose description carries the accountant's reason.
--
-- The API applies the same forward-only change at runtime
-- (ensure_wallet_deduction_schema in operations.rs) for databases the
-- migrator cannot reach. Safe to run more than once.
DO $$
DECLARE
    constraint_name text;
BEGIN
    FOR constraint_name IN
        SELECT con.conname
        FROM pg_constraint con
        WHERE con.conrelid = 'campus_ops.canteen_wallets'::regclass
          AND con.contype = 'c'
          AND pg_get_constraintdef(con.oid) ILIKE '%balance >=%'
    LOOP
        EXECUTE format('ALTER TABLE campus_ops.canteen_wallets DROP CONSTRAINT %I', constraint_name);
    END LOOP;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint con
        WHERE con.conrelid = 'campus_ops.canteen_wallet_transactions'::regclass
          AND con.contype = 'c'
          AND pg_get_constraintdef(con.oid) ILIKE '%transaction_type%'
          AND pg_get_constraintdef(con.oid) ILIKE '%manual_debit%'
    ) THEN
        FOR constraint_name IN
            SELECT con.conname
            FROM pg_constraint con
            WHERE con.conrelid = 'campus_ops.canteen_wallet_transactions'::regclass
              AND con.contype = 'c'
              AND pg_get_constraintdef(con.oid) ILIKE '%transaction_type%'
        LOOP
            EXECUTE format(
                'ALTER TABLE campus_ops.canteen_wallet_transactions DROP CONSTRAINT %I',
                constraint_name
            );
        END LOOP;
        ALTER TABLE campus_ops.canteen_wallet_transactions
            ADD CONSTRAINT canteen_wallet_transactions_transaction_type_check
            CHECK (transaction_type IN
                ('manual_top_up', 'online_top_up', 'order_debit', 'refund', 'manual_debit'));
    END IF;
END $$;
