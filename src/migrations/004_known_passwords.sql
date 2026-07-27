-- Set known passwords for test accounts
-- Password for arun.kumar@svce.edu.in -> Student@123
UPDATE students
SET password_hash = '$2b$12$d5PO6lSBKJ4L.wsKi/Mo9.P0WYkSZYxFkoTKgZZ.z83F0rvEQ4Ytu',
    updated_at = now()
WHERE email = 'arun.kumar@svce.edu.in';

-- Password for priya.sharma@rec.edu.in -> Campus@123
UPDATE students
SET password_hash = '$2b$12$Pi96zpZNaz5LgsGJYf9UhudoDlZnX6XUlMFbLB6/utZMTDDlUOvla',
    updated_at = now()
WHERE email = 'priya.sharma@rec.edu.in';
