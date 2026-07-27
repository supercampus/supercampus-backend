import pg from 'pg';
import { config } from './config.js';

const { Pool } = pg;

export const pool = new Pool({
  connectionString: config.databaseUrl,
  max: config.poolMax,
  idleTimeoutMillis: 30_000,
  connectionTimeoutMillis: 5_000,
  ssl: config.databaseSsl === 'disable'
    ? false
    : { rejectUnauthorized: config.databaseSsl === 'verify-full' },
});

pool.on('error', (error) => {
  console.error('Unexpected PostgreSQL pool error', error);
});

export async function query(text, values = []) {
  return pool.query(text, values);
}

export async function withTransaction(callback) {
  const client = await pool.connect();
  try {
    await client.query('BEGIN');
    const result = await callback(client);
    await client.query('COMMIT');
    return result;
  } catch (error) {
    await client.query('ROLLBACK');
    throw error;
  } finally {
    client.release();
  }
}
