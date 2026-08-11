-- Baseline dynamic portal permissions and role presets.
--
-- The admin web console, backend auth, and mobile app all meet at these
-- permission keys. A role with no grants may still sign in, but the app will
-- correctly show no modules; these presets make the common institution roles
-- usable immediately.

INSERT INTO authz.permission_templates
    (permission_key, module_key, feature_key, action, display_name, description, crud_actions)
SELECT permission.permission_key, permission.module_key, permission.feature_key,
       permission.action, permission.display_name, permission.description,
       permission.crud_actions
FROM (VALUES
    ('examination.dashboard.read', 'examination', 'dashboard', 'read', 'View exam dashboard', 'View examination dashboard and summary', ARRAY['read']::text[]),
    ('examination.scheduling.read', 'examination', 'scheduling', 'read', 'View exam schedule', 'View published exam schedules', ARRAY['read']::text[]),
    ('examination.eligibility.read', 'examination', 'eligibility', 'read', 'View exam eligibility', 'View exam eligibility status', ARRAY['read']::text[]),
    ('examination.grades.read', 'examination', 'grades', 'read', 'View grades', 'View grades and GPA', ARRAY['read']::text[]),
    ('examination.publishing.read', 'examination', 'publishing', 'read', 'View published results', 'View published examination results', ARRAY['read']::text[]),
    ('examination.revaluation.create', 'examination', 'revaluation', 'create', 'Request revaluation', 'Create revaluation requests', ARRAY['create']::text[]),
    ('examination.revaluation.read', 'examination', 'revaluation', 'read', 'View revaluation', 'View revaluation requests', ARRAY['read']::text[]),
    ('examination.transcript.read', 'examination', 'transcript', 'read', 'View transcript', 'View transcripts', ARRAY['read']::text[]),
    ('examination.reports.read', 'examination', 'reports', 'read', 'View exam reports', 'View examination reports', ARRAY['read']::text[]),

    ('timetable.schedule.read', 'timetable', 'schedule', 'read', 'View timetable', 'View class timetable', ARRAY['read']::text[]),
    ('timetable.substitution.read', 'timetable', 'substitution', 'read', 'View substitutions', 'View timetable substitutions', ARRAY['read']::text[]),
    ('timetable.publication.read', 'timetable', 'publication', 'read', 'View timetable publications', 'View published timetable changes', ARRAY['read']::text[]),

    ('attendance.roster.read', 'attendance', 'roster', 'read', 'View attendance roster', 'View attendance roster', ARRAY['read']::text[]),
    ('attendance.swipe.create', 'attendance', 'swipe', 'create', 'Record attendance swipe', 'Create attendance swipe entries', ARRAY['create']::text[]),
    ('attendance.swipe.read', 'attendance', 'swipe', 'read', 'View attendance swipes', 'View attendance swipe log', ARRAY['read']::text[]),
    ('attendance.leave.create', 'attendance', 'leave', 'create', 'Request leave', 'Create attendance leave requests', ARRAY['create']::text[]),
    ('attendance.leave.read', 'attendance', 'leave', 'read', 'View leave', 'View attendance leave requests', ARRAY['read']::text[]),
    ('attendance.leave.approve', 'attendance', 'leave', 'approve', 'Approve leave', 'Approve attendance leave requests', ARRAY['update']::text[]),

    ('canteen.menu.read', 'canteen', 'menu', 'read', 'View canteen menu', 'View canteen menu', ARRAY['read']::text[]),
    ('canteen.order.create', 'canteen', 'order', 'create', 'Create canteen order', 'Create canteen orders', ARRAY['create']::text[]),
    ('canteen.order.read', 'canteen', 'order', 'read', 'View canteen orders', 'View canteen orders', ARRAY['read']::text[]),
    ('canteen.order.update', 'canteen', 'order', 'update', 'Update canteen order', 'Update canteen orders', ARRAY['update']::text[]),
    ('canteen.wallet.read', 'canteen', 'wallet', 'read', 'View wallet', 'View canteen wallet balance', ARRAY['read']::text[]),
    ('canteen.wallet.update', 'canteen', 'wallet', 'update', 'Update wallet', 'Update canteen wallet balance', ARRAY['update']::text[]),

    ('gatepass.outpass.create', 'gatepass', 'outpass', 'create', 'Create outpass', 'Create gatepass outpass requests', ARRAY['create']::text[]),
    ('gatepass.outpass.read', 'gatepass', 'outpass', 'read', 'View outpasses', 'View gatepass outpass requests', ARRAY['read']::text[]),
    ('gatepass.outpass.update', 'gatepass', 'outpass', 'update', 'Update outpass', 'Update gatepass outpass requests', ARRAY['update']::text[]),
    ('gatepass.outpass.approve', 'gatepass', 'outpass', 'approve', 'Approve outpass', 'Approve gatepass outpass requests', ARRAY['update']::text[]),
    ('gatepass.outpass.reject', 'gatepass', 'outpass', 'reject', 'Reject outpass', 'Reject gatepass outpass requests', ARRAY['update']::text[]),
    ('gatepass.outpass.verify', 'gatepass', 'outpass', 'verify', 'Verify outpass', 'Verify gatepass at security checkpoint', ARRAY['update']::text[]),
    ('gatepass.visitor.create', 'gatepass', 'visitor', 'create', 'Create visitor invite', 'Create gatepass visitor invitations', ARRAY['create']::text[]),
    ('gatepass.visitor.read', 'gatepass', 'visitor', 'read', 'View visitor invites', 'View gatepass visitor invitations', ARRAY['read']::text[]),
    ('gatepass.access.read', 'gatepass', 'access', 'read', 'View gate access', 'View gate access status', ARRAY['read']::text[]),
    ('gatepass.access.update', 'gatepass', 'access', 'update', 'Update gate access', 'Update gate access status', ARRAY['update']::text[]),

    ('library.visit_pass.create', 'library', 'visit_pass', 'create', 'Book library visit', 'Create library visit passes', ARRAY['create']::text[]),
    ('library.visit_pass.read', 'library', 'visit_pass', 'read', 'View library visit pass', 'View library visit passes', ARRAY['read']::text[]),
    ('library.qr_pass.read', 'library', 'qr_pass', 'read', 'View library QR pass', 'View library QR passes', ARRAY['read']::text[]),
    ('library.visit_history.read', 'library', 'visit_history', 'read', 'View library history', 'View library visit history', ARRAY['read']::text[]),
    ('library.occupancy.read', 'library', 'occupancy', 'read', 'View library occupancy', 'View library occupancy and capacity', ARRAY['read']::text[]),

    ('tuition_fee.invoice.read', 'tuition_fee', 'invoice', 'read', 'View fee invoices', 'View tuition fee invoices', ARRAY['read']::text[]),
    ('tuition_fee.payment.create', 'tuition_fee', 'payment', 'create', 'Create fee payment', 'Create tuition fee payments', ARRAY['create']::text[]),
    ('tuition_fee.payment.read', 'tuition_fee', 'payment', 'read', 'View fee payments', 'View tuition fee payments', ARRAY['read']::text[]),

    ('academics.attendance.read', 'academics', 'attendance', 'read', 'View academic attendance', 'View academic attendance', ARRAY['read']::text[]),
    ('academics.marks.read', 'academics', 'marks', 'read', 'View academic marks', 'View academic marks', ARRAY['read']::text[]),
    ('academics.analysis.read', 'academics', 'analysis', 'read', 'View academic analysis', 'View academic analysis', ARRAY['read']::text[]),
    ('academics.programme.read', 'academics', 'programme', 'read', 'View programmes', 'View academic programmes', ARRAY['read']::text[]),
    ('academics.subject.read', 'academics', 'subject', 'read', 'View subjects', 'View academic subjects', ARRAY['read']::text[])
) AS permission(permission_key, module_key, feature_key, action, display_name, description, crud_actions)
ON CONFLICT (permission_key) DO UPDATE
SET module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    crud_actions = EXCLUDED.crud_actions,
    active = true,
    updated_at = now();

INSERT INTO authz.permission_definitions
    (tenant_id, permission_key, module_key, feature_key, action, display_name, description, crud_actions)
SELECT tenant.id, template.permission_key, template.module_key, template.feature_key,
       template.action, template.display_name, template.description, template.crud_actions
FROM platform.tenants AS tenant
CROSS JOIN authz.permission_templates AS template
WHERE template.active
ON CONFLICT (tenant_id, permission_key) DO UPDATE
SET module_key = EXCLUDED.module_key,
    feature_key = EXCLUDED.feature_key,
    action = EXCLUDED.action,
    display_name = EXCLUDED.display_name,
    description = EXCLUDED.description,
    crud_actions = EXCLUDED.crud_actions,
    active = true,
    updated_at = now();

INSERT INTO authz.roles
    (tenant_id, role_key, name, team, scope_description, protected, created_by, updated_by)
SELECT tenant.id, preset.role_key, preset.name, preset.team, preset.scope_description,
       false, 'runtime-migration-0032', 'runtime-migration-0032'
FROM platform.tenants tenant
CROSS JOIN (VALUES
    ('student', 'Student', 'Students', 'Student portal access across academics, attendance, canteen, gatepass, library, fees, timetable, and examinations'),
    ('parent', 'Parent / Guardian', 'Students', 'Guardian portal access for student status and gatepass approvals'),
    ('warden', 'Warden', 'Hostel', 'Reviews and approves student outpass requests'),
    ('security', 'Security', 'Security', 'Verifies gatepass movement and gate access at campus exits')
) AS preset(role_key, name, team, scope_description)
ON CONFLICT (tenant_id, role_key) DO UPDATE
SET name = EXCLUDED.name,
    team = EXCLUDED.team,
    scope_description = EXCLUDED.scope_description,
    active = true,
    updated_by = 'runtime-migration-0032',
    updated_at = now();

INSERT INTO authz.role_permissions
    (tenant_id, role_id, permission_key, scope, constraints, granted_by)
SELECT role.tenant_id, role.id, preset_grant.permission_key, preset_grant.scope, '{}'::jsonb,
       'runtime-migration-0032'
FROM authz.roles role
JOIN (VALUES
    ('student', 'examination.dashboard.read', 'own'),
    ('student', 'examination.scheduling.read', 'own'),
    ('student', 'examination.eligibility.read', 'own'),
    ('student', 'examination.grades.read', 'own'),
    ('student', 'examination.publishing.read', 'own'),
    ('student', 'examination.revaluation.create', 'own'),
    ('student', 'examination.revaluation.read', 'own'),
    ('student', 'examination.transcript.read', 'own'),
    ('student', 'examination.reports.read', 'own'),
    ('student', 'timetable.schedule.read', 'own'),
    ('student', 'timetable.substitution.read', 'own'),
    ('student', 'timetable.publication.read', 'own'),
    ('student', 'attendance.roster.read', 'own'),
    ('student', 'attendance.swipe.create', 'own'),
    ('student', 'attendance.swipe.read', 'own'),
    ('student', 'attendance.leave.create', 'own'),
    ('student', 'attendance.leave.read', 'own'),
    ('student', 'canteen.menu.read', 'own'),
    ('student', 'canteen.order.create', 'own'),
    ('student', 'canteen.order.read', 'own'),
    ('student', 'canteen.order.update', 'own'),
    ('student', 'canteen.wallet.read', 'own'),
    ('student', 'gatepass.outpass.create', 'own'),
    ('student', 'gatepass.outpass.read', 'own'),
    ('student', 'gatepass.outpass.update', 'own'),
    ('student', 'gatepass.visitor.create', 'own'),
    ('student', 'gatepass.visitor.read', 'own'),
    ('student', 'gatepass.access.read', 'own'),
    ('student', 'library.visit_pass.create', 'own'),
    ('student', 'library.visit_pass.read', 'own'),
    ('student', 'library.qr_pass.read', 'own'),
    ('student', 'library.visit_history.read', 'own'),
    ('student', 'library.occupancy.read', 'own'),
    ('student', 'tuition_fee.invoice.read', 'own'),
    ('student', 'tuition_fee.payment.create', 'own'),
    ('student', 'tuition_fee.payment.read', 'own'),
    ('student', 'academics.attendance.read', 'own'),
    ('student', 'academics.marks.read', 'own'),
    ('student', 'academics.analysis.read', 'own'),
    ('student', 'academics.programme.read', 'own'),
    ('student', 'academics.subject.read', 'own'),

    ('parent', 'academics.attendance.read', 'own'),
    ('parent', 'academics.marks.read', 'own'),
    ('parent', 'academics.analysis.read', 'own'),
    ('parent', 'attendance.leave.read', 'own'),
    ('parent', 'gatepass.outpass.read', 'own'),
    ('parent', 'gatepass.outpass.approve', 'own'),
    ('parent', 'tuition_fee.invoice.read', 'own'),
    ('parent', 'tuition_fee.payment.read', 'own'),

    ('warden', 'gatepass.outpass.read', 'assigned'),
    ('warden', 'gatepass.outpass.approve', 'assigned'),
    ('warden', 'gatepass.outpass.reject', 'assigned'),
    ('warden', 'gatepass.visitor.read', 'assigned'),
    ('warden', 'attendance.leave.read', 'assigned'),
    ('warden', 'attendance.leave.approve', 'assigned'),

    ('security', 'gatepass.outpass.read', 'assigned'),
    ('security', 'gatepass.outpass.verify', 'assigned'),
    ('security', 'gatepass.access.read', 'assigned'),
    ('security', 'gatepass.access.update', 'assigned'),
    ('security', 'gatepass.visitor.read', 'assigned')
) AS preset_grant(role_key, permission_key, scope)
  ON preset_grant.role_key = role.role_key
JOIN authz.permission_definitions permission
  ON permission.tenant_id = role.tenant_id
 AND permission.permission_key = preset_grant.permission_key
 AND permission.active
ON CONFLICT (tenant_id, role_id, permission_key) DO UPDATE
SET scope = EXCLUDED.scope,
    constraints = EXCLUDED.constraints,
    granted_by = EXCLUDED.granted_by,
    granted_at = now();
