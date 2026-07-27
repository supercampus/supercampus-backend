import 'dotenv/config';
import pg from 'pg';

const pool = new pg.Pool({ connectionString: process.env.DATABASE_URL, max: 2 });
try {
  const [counts, tenants, students, states, activity, migrations] = await Promise.all([
    pool.query(`
      select 'tenants' as table_name, count(*)::int as rows from tenants
      union all select 'students', count(*)::int from students
      union all select 'student_app_state', count(*)::int from student_app_state
      union all select 'activity_events', count(*)::int from activity_events
      union all select 'schema_migrations', count(*)::int from schema_migrations
      order by table_name
    `),
    pool.query('select code, name, city, is_active as active, created_at as "createdAt" from tenants order by code'),
    pool.query(`
      select t.code as tenant, s.email, s.roll_number as roll, s.name,
             s.department, s.study_year as year, s.is_active as active,
             length(s.password_hash) > 20 as "passwordConfigured",
             s.created_at as "createdAt", s.updated_at as "updatedAt"
      from students s join tenants t on t.id = s.tenant_id
      order by t.code, s.roll_number
    `),
    pool.query(`
      select t.code as tenant, s.email, sas.version, sas.updated_at as "updatedAt",
             sas.state->>'persona' as persona,
             sas.state->'gp' as "gatePass",
             sas.state->'paid' as paid,
             jsonb_array_length(coalesce(sas.state->'docReq', '[]'::jsonb)) as "documentRequests",
             jsonb_array_length(coalesce(sas.state->'hostelTickets', '[]'::jsonb)) as "hostelTickets",
             sas.state->>'condonation' as condonation,
             sas.state->>'examReg' as "examRegistration",
             sas.state->>'placeApp' as "placementApplications"
      from student_app_state sas
      join students s on s.id = sas.student_id
      join tenants t on t.id = s.tenant_id
      order by t.code
    `),
    pool.query(`
      select t.code as tenant, s.email, ae.event_type as "eventType",
             ae.metadata, ae.created_at as "createdAt"
      from activity_events ae
      join students s on s.id = ae.student_id
      join tenants t on t.id = s.tenant_id
      order by ae.created_at desc limit 20
    `),
    pool.query('select name, applied_at as "appliedAt" from schema_migrations order by name'),
  ]);
  console.log(JSON.stringify({
    tableCounts: counts.rows,
    tenants: tenants.rows,
    students: students.rows,
    studentStateSummary: states.rows,
    recentActivity: activity.rows,
    migrations: migrations.rows,
  }, null, 2));
} finally {
  await pool.end();
}
