-- A class advisor reviews only departments explicitly assigned through
-- core.class_advisor_assignments. Use the API's assigned scope branch rather
-- than the HOD-only department_authorities branch.
UPDATE authz.role_permissions role_permission
   SET scope='assigned',
       granted_by='runtime-migration-0081',
       granted_at=now()
  FROM authz.roles role
 WHERE role_permission.tenant_id=role.tenant_id
   AND role_permission.role_id=role.id
   AND role.role_key='class_advisor'
   AND role_permission.permission_key IN (
     'attendance.roster.read', 'attendance.roster.update',
     'attendance.session.create', 'attendance.session.publish',
     'attendance.records.read', 'attendance.reports.create',
     'attendance.reports.publish'
   );
