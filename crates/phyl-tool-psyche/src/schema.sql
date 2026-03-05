CREATE TABLE IF NOT EXISTS concerns (
    concern_id       TEXT PRIMARY KEY,
    description      TEXT NOT NULL,
    type             TEXT NOT NULL CHECK(type IN ('epistemic','appetitive','conative')),
    tension          TEXT,
    state            TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','committed','resolved','abandoned')),
    salience         REAL NOT NULL DEFAULT 0.5,
    tags             TEXT NOT NULL DEFAULT '[]',
    origin           TEXT NOT NULL DEFAULT 'session',
    touch_count      INTEGER NOT NULL DEFAULT 0,
    created_session  INTEGER NOT NULL,
    touched_session  INTEGER NOT NULL,
    created_at       TEXT NOT NULL,
    touched_at       TEXT NOT NULL,
    resolved_at      TEXT,
    abandoned_at     TEXT,
    outcome          TEXT,
    abandon_reason   TEXT,
    spawned_from     TEXT REFERENCES concerns(concern_id),
    spawned          TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS commitments (
    commitment_id    TEXT PRIMARY KEY,
    concern_id       TEXT NOT NULL REFERENCES concerns(concern_id),
    action           TEXT NOT NULL,
    scheduled_for    TEXT NOT NULL,
    fallback         TEXT,
    state            TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','fulfilled','broken')),
    created_at       TEXT NOT NULL,
    reported_at      TEXT,
    note             TEXT,
    spawned_concerns TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS escalations (
    escalation_id    TEXT PRIMARY KEY,
    subject          TEXT NOT NULL,
    body             TEXT NOT NULL,
    urgency          TEXT NOT NULL DEFAULT 'normal',
    kind             TEXT NOT NULL,
    concern_id       TEXT REFERENCES concerns(concern_id),
    commitment_id    TEXT REFERENCES commitments(commitment_id),
    blocking_action  TEXT,
    proposed_resolution TEXT,
    created_at       TEXT NOT NULL,
    responded_at     TEXT,
    response         TEXT
);

CREATE TABLE IF NOT EXISTS kb_records (
    record_id        TEXT PRIMARY KEY,
    subject          TEXT NOT NULL,
    predicate        TEXT NOT NULL,
    object           TEXT NOT NULL,
    confidence       REAL NOT NULL,
    source           TEXT NOT NULL,
    concern_id       TEXT REFERENCES concerns(concern_id),
    created_at       TEXT NOT NULL,
    expires_at       TEXT,
    invalidated_at   TEXT,
    invalidation_reason TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
    session_number   INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT NOT NULL,
    began_at         TEXT NOT NULL,
    closed_at        TEXT
);

CREATE TABLE IF NOT EXISTS briefings (
    briefing_id      TEXT PRIMARY KEY,
    session_number   INTEGER NOT NULL,
    generated_at     TEXT NOT NULL,
    content          TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_concerns_state ON concerns(state);
CREATE INDEX IF NOT EXISTS idx_concerns_salience ON concerns(salience DESC);
CREATE INDEX IF NOT EXISTS idx_commitments_state ON commitments(state);
CREATE INDEX IF NOT EXISTS idx_kb_records_subject ON kb_records(subject);
CREATE INDEX IF NOT EXISTS idx_escalations_responded ON escalations(responded_at);
