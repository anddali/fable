-- Fails with a unique violation if this migration is ever applied twice.
INSERT INTO widgets (id) VALUES (1);
