ALTER TABLE crm.communications
    DROP CONSTRAINT IF EXISTS communications_channel_check;
ALTER TABLE crm.communications
    ADD CONSTRAINT communications_channel_check
    CHECK (channel IN ('whatsapp', 'email', 'call', 'sms', 'note'));
