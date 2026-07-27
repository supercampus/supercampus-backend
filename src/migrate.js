import { readdir, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { pool } from './db.js';

const currentDir = dirname(fileURLToPath(import.meta.url));
const migrationsDir = join(currentDir, 'migrations');
const migrationLockId = 1_924_637_551;
const client = await pool.connect();
let lockAcquired = false;

try {
  await client.query('SELECT pg_advisory_lock($1)', [migrationLockId]);
  lockAcquired = true;
  await client.query(`
    CREATE TABLE IF NOT EXISTS schema_migrations (
      name text PRIMARY KEY,
      applied_at timestamptz NOT NULL DEFAULT now()
    )
  `);

  const files = (await readdir(migrationsDir)).filter((file) => file.endsWith('.sql')).sort();
  for (const file of files) {
    const alreadyApplied = await client.query('SELECT 1 FROM schema_migrations WHERE name = $1', [file]);
    if (alreadyApplied.rowCount) continue;

    const sql = await readFile(join(migrationsDir, file), 'utf8');
    try {
      await client.query('BEGIN');
      await client.query(sql);
      await client.query('INSERT INTO schema_migrations (name) VALUES ($1)', [file]);
      await client.query('COMMIT');
      console.log(`Applied ${file}`);
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    }
  }

  const verification = await client.query(
    "select table_name from information_schema.tables where table_schema = 'public' and table_name = any($1::text[]) order by table_name",
    [['tenants', 'students', 'student_app_state', 'activity_events', 'schema_migrations']],
  );
  console.log('Migration verification:', verification.rows.map((row) => row.table_name).join(', '));
  console.log('Database migrations completed');
} finally {
  if (lockAcquired) {
    try {
      await client.query('SELECT pg_advisory_unlock($1)', [migrationLockId]);
    } catch (error) {
      console.error('Failed to release migration lock cleanly', error);
    }
  }
  client.release();
  await pool.end();
}
