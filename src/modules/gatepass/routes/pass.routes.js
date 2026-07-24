'use strict';

const express    = require('express');
const router     = express.Router();
const ctrl       = require('../controllers/pass.controller');
const { requireRoles } = require('../middleware/gatepass.auth');

// POST   /api/gatepass/passes          — submit a new pass request
router.post('/', requireRoles(['STUDENT', 'STAFF', 'SECURITY']), ctrl.createPass);

// GET    /api/gatepass/passes          — list own passes (role-filtered)
router.get('/', requireRoles(['STUDENT', 'STAFF', 'ADMIN', 'SECURITY']), ctrl.listPasses);

// GET    /api/gatepass/passes/:id      — get single pass with approval chain
router.get('/:id', requireRoles(['STUDENT', 'STAFF', 'ADMIN', 'SECURITY']), ctrl.getPass);

// DELETE /api/gatepass/passes/:id      — cancel a pending pass (owner only)
router.delete('/:id', requireRoles(['STUDENT', 'STAFF']), ctrl.cancelPass);

module.exports = router;
