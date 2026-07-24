'use strict';

const express = require('express');
const router = express.Router();

const passRoutes     = require('./routes/pass.routes');
const approvalRoutes = require('./routes/approval.routes');
const qrRoutes       = require('./routes/qr.routes');
const scanRoutes     = require('./routes/scan.routes');
const geofenceRoutes = require('./routes/geofence.routes');
const overrideRoutes = require('./routes/override.routes');

router.use('/passes',   passRoutes);
router.use('/approvals', approvalRoutes);
router.use('/qr',       qrRoutes);
router.use('/scan',     scanRoutes);
router.use('/geofence', geofenceRoutes);
router.use('/override', overrideRoutes);

module.exports = router;
