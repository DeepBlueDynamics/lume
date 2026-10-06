CREATE TABLE telemetry(vessel VARCHAR, ts TIMESTAMP, speed DOUBLE, state VARCHAR, "speed$source" VARCHAR[]);
INSERT INTO telemetry VALUES
('vessels.urn:test:1', TIMESTAMP '2020-01-01 00:00:00', -1, 'off', ['gps','ais']),
('vessels.urn:test:1', TIMESTAMP '2020-01-01 00:00:01', 0, 'on', ['ais']),
('vessels.urn:test:1', TIMESTAMP '2020-01-01 00:00:02', 1.25, 'on', NULL),
('vessels.urn:test:1', TIMESTAMP '2020-01-01 00:00:03', NULL, NULL, NULL);
