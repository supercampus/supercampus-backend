'use strict';

const express      = require('express');
const gatepassRouter = require('./modules/gatepass');

const app = express();

app.use(express.json());

// ─── Health ───────────────────────────────────────────────────────────────────
app.get('/', (req, res) => {
  res.json({ message: 'Express API is running' });
});

app.get('/health', (req, res) => {
  res.json({ status: 'ok' });
});

// ─── Modules ──────────────────────────────────────────────────────────────────
app.use('/api/gatepass', gatepassRouter);

module.exports = app;
