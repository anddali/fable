-- Slow on purpose so concurrent runners overlap.
CREATE TABLE widgets (id int PRIMARY KEY);
SELECT pg_sleep(0.5);
